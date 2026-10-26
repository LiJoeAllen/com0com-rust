//! State API bound to one backend and one state file.
//!
//! The persistence semantics live in [`com0com_core::state_io`] so the daemon
//! and the in-process SDK behave identically.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use com0com_core::state_io;
use com0com_core::{Backend, RestoreOptions, RestoreReport, SdkError, Snapshot};

pub use com0com_core::default_state_path;

enum Source {
    /// In-process backend; persistence runs here.
    Local(Arc<dyn Backend>),
    /// Daemon connection; persistence runs in the daemon that owns the
    /// devices, so every state method is an RPC.
    Remote(Arc<com0com_ipc::RemoteBackend>),
}

pub struct StateApi {
    source: Source,
    path: PathBuf,
}

impl StateApi {
    pub fn new(backend: Arc<dyn Backend>, path: impl Into<PathBuf>) -> Self {
        Self {
            source: Source::Local(backend),
            path: path.into(),
        }
    }

    /// State API whose save/load/restore operations run inside the daemon.
    pub fn remote(remote: Arc<com0com_ipc::RemoteBackend>, path: impl Into<PathBuf>) -> Self {
        Self {
            source: Source::Remote(remote),
            path: path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Capture the current pairs.
    pub fn snapshot(&self) -> Result<Snapshot, SdkError> {
        match &self.source {
            Source::Local(backend) => state_io::capture(&**backend),
            Source::Remote(remote) => remote.snapshot(),
        }
    }

    /// Capture and atomically write to the configured state file (the
    /// daemon's file when connected over IPC).
    pub fn save(&self) -> Result<Snapshot, SdkError> {
        match &self.source {
            Source::Local(backend) => state_io::save(&**backend, &self.path),
            Source::Remote(remote) => remote.save_state().map(|saved| saved.snapshot),
        }
    }

    /// Atomically write `snapshot` to `path` in this process. Works for both
    /// local and daemon backends and never writes the daemon's file.
    pub fn save_to(&self, path: &Path, snapshot: &Snapshot) -> Result<(), SdkError> {
        state_io::save_to(path, snapshot)
    }

    /// Read and validate the configured state file.
    pub fn load(&self) -> Result<Snapshot, SdkError> {
        match &self.source {
            Source::Local(_) => Self::load_from(&self.path),
            Source::Remote(remote) => remote.load_state(),
        }
    }

    pub fn load_from(path: &Path) -> Result<Snapshot, SdkError> {
        state_io::load(path)
    }

    pub fn export_json(&self) -> Result<String, SdkError> {
        match &self.source {
            Source::Local(backend) => state_io::export_json(&**backend),
            Source::Remote(remote) => remote.export_state(),
        }
    }

    pub fn import_json(
        &self,
        json: &str,
        options: RestoreOptions,
    ) -> Result<RestoreReport, SdkError> {
        match &self.source {
            Source::Local(backend) => state_io::import_json(&**backend, json, options),
            Source::Remote(remote) => remote.import_state(json, options),
        }
    }

    /// Reconcile the backend with `snapshot`.
    pub fn restore(
        &self,
        snapshot: &Snapshot,
        options: RestoreOptions,
    ) -> Result<RestoreReport, SdkError> {
        match &self.source {
            Source::Local(backend) => state_io::restore(&**backend, snapshot, options),
            Source::Remote(remote) => remote.restore_state(snapshot, options),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use com0com_core::fake::FakeBackend;
    use com0com_core::{KernelKind, PairParams, PortName, SnapshotPair};
    use std::fs;
    use std::sync::atomic::AtomicU32;

    static DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let unique = DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "com0com-state-test-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn params(a: &str, b: &str) -> PairParams {
        PairParams {
            port_a: PortName::new(a).unwrap(),
            port_b: PortName::new(b).unwrap(),
            ..PairParams::default()
        }
    }

    fn setup() -> (Arc<FakeBackend>, StateApi, TestDir) {
        let backend = Arc::new(FakeBackend::new());
        let dir = TestDir::new();
        backend.create_pair(Some(0), &params("A0", "B0")).unwrap();
        backend.create_pair(Some(2), &params("C2", "D2")).unwrap();
        let api = StateApi::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            dir.join("state.json"),
        );
        (backend, api, dir)
    }

    #[test]
    fn snapshot_roundtrip_and_atomic_replace() {
        let (_backend, api, _dir) = setup();
        let snapshot = api.save().unwrap();
        assert_eq!(snapshot.pairs.len(), 2);
        assert!(api.path().exists());

        let loaded = api.load().unwrap();
        assert_eq!(loaded, snapshot);

        // Saving again replaces the file in place (rename over an existing file)
        // and leaves no temporary files behind. `created_at` is a fresh
        // timestamp, so only the portable fields are compared.
        let first = api.snapshot().unwrap();
        let report = api.save().unwrap();
        assert_eq!(report.pairs, first.pairs);
        let reloaded = api.load().unwrap();
        assert_eq!(reloaded.pairs, first.pairs);
        let leftovers: Vec<_> = fs::read_dir(api.path().parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn restore_reports_and_applies_the_diff() {
        let (backend, api, _dir) = setup();
        let snapshot = api.snapshot().unwrap();

        backend.set_params(0, &params("A0x", "B0")).unwrap();
        backend.delete_pair(2).unwrap();
        backend.create_pair(Some(5), &params("E5", "F5")).unwrap();

        let dry = RestoreOptions {
            dry_run: true,
            remove_extra: true,
            continue_on_error: false,
        };
        let report = api.restore(&snapshot, dry).unwrap();
        assert!(report.dry_run);
        assert!(report.is_success());
        assert_eq!(report.applied, vec![5, 0, 2]);
        // Nothing was touched.
        assert_eq!(backend.get_params(0).unwrap().port_a.as_str(), "A0x");
        assert!(backend.get_params(2).is_err());
        assert!(backend.get_params(5).is_ok());

        let apply = RestoreOptions {
            dry_run: false,
            remove_extra: true,
            continue_on_error: false,
        };
        let report = api.restore(&snapshot, apply).unwrap();
        assert!(!report.dry_run);
        assert!(report.is_success(), "{report:?}");
        assert_eq!(report.applied, vec![5, 0, 2]);
        assert_eq!(backend.get_params(0).unwrap().port_a.as_str(), "A0");
        assert_eq!(backend.get_params(2).unwrap().port_b.as_str(), "D2");
        assert!(backend.get_params(5).is_err());
    }

    #[test]
    fn restore_without_remove_extra_keeps_extras_and_reports_conflicts() {
        let backend = Arc::new(FakeBackend::new());
        backend.create_pair(Some(0), &params("A", "B")).unwrap();
        let dir = TestDir::new();
        let api = StateApi::new(
            Arc::clone(&backend) as Arc<dyn Backend>,
            dir.join("state.json"),
        );
        let snapshot = Snapshot::new(
            "fake",
            KernelKind::None,
            vec![SnapshotPair {
                id: 1,
                params: params("A", "C"),
            }],
        );
        let report = api
            .restore(
                &snapshot,
                RestoreOptions {
                    dry_run: false,
                    remove_extra: false,
                    continue_on_error: false,
                },
            )
            .unwrap();
        assert!(!report.is_success());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].pair, 1);
        assert_eq!(backend.list_pairs().unwrap().len(), 1);

        let report = api
            .restore(
                &snapshot,
                RestoreOptions {
                    dry_run: true,
                    remove_extra: false,
                    continue_on_error: false,
                },
            )
            .unwrap();
        assert!(report.is_success());
        assert_eq!(report.applied, vec![1]);
    }

    #[test]
    fn import_rejects_wrong_schema_and_bad_json() {
        let (_backend, api, _dir) = setup();
        assert!(matches!(
            api.import_json("{", RestoreOptions::default()),
            Err(SdkError::Protocol(_))
        ));
        let mut snapshot = api.snapshot().unwrap();
        snapshot.schema = 3;
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(matches!(
            api.import_json(&json, RestoreOptions::default()),
            Err(SdkError::Protocol(_))
        ));
    }
}
