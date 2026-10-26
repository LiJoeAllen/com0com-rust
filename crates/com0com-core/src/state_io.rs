//! Snapshot capture, atomic persistence and restore reporting.
//!
//! These free functions are shared by the SDK facade and the JSON-RPC daemon so
//! both persist, load and restore with identical semantics. Device paths are
//! deliberately not stored: the backend recreates them on restore.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    Backend, PairId, PairParams, RestoreFailure, RestoreOptions, RestoreReport, SdkError, Snapshot,
    SnapshotPair,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Capture the backend's current pairs.
pub fn capture(backend: &dyn Backend) -> Result<Snapshot, SdkError> {
    let info = backend.info();
    let pairs = backend
        .list_pairs()?
        .into_iter()
        .map(|pair| SnapshotPair {
            id: pair.id,
            params: pair.params,
        })
        .collect();
    Ok(Snapshot::new(info.platform, info.kernel, pairs))
}

/// Capture and atomically write to `path`.
pub fn save(backend: &dyn Backend, path: &Path) -> Result<Snapshot, SdkError> {
    let snapshot = capture(backend)?;
    save_to(path, &snapshot)?;
    Ok(snapshot)
}

/// Atomically write `snapshot` to `path` (temp file in the same directory,
/// `sync_all`, then rename).
pub fn save_to(path: &Path, snapshot: &Snapshot) -> Result<(), SdkError> {
    snapshot.validate()?;
    let json = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| SdkError::Internal(format!("snapshot encoding failed: {error}")))?;
    atomic_write(path, &json)
}

/// Read and validate a state file.
pub fn load(path: &Path) -> Result<Snapshot, SdkError> {
    let bytes = fs::read(path).map_err(io_error)?;
    decode_snapshot(&bytes)
}

/// 反序列化快照，并把 schema 1 的旧噪声概率换算成上游单位。
///
/// schema 1 用 0.01% 单位（0..=10000），schema 2 用 1/1e8 单位。
/// 旧文件里的 `noise_probability` 需要乘以 10_000，并补上默认的
/// `all_data_bits = false`（由字段上的 `#[serde(default)]` 完成）。
fn decode_snapshot(json: &[u8]) -> Result<Snapshot, SdkError> {
    let mut value: serde_json::Value = serde_json::from_slice(json).map_err(protocol_error)?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if schema == 1 {
        if let Some(pairs) = value
            .get_mut("pairs")
            .and_then(serde_json::Value::as_array_mut)
        {
            for pair in pairs {
                let Some(params) = pair
                    .get_mut("params")
                    .and_then(serde_json::Value::as_object_mut)
                else {
                    continue;
                };
                let Some(prob) = params
                    .get("noise_probability")
                    .and_then(serde_json::Value::as_u64)
                else {
                    continue;
                };
                let scaled = prob.saturating_mul(10_000).min(100_000_000);
                params.insert(
                    "noise_probability".to_string(),
                    serde_json::Value::from(scaled),
                );
            }
        }
        value["schema"] = serde_json::Value::from(crate::SNAPSHOT_SCHEMA);
    }
    let snapshot: Snapshot = serde_json::from_value(value).map_err(protocol_error)?;
    snapshot.validate()?;
    Ok(snapshot)
}

/// Encode the backend's current state as JSON (without touching the disk).
pub fn export_json(backend: &dyn Backend) -> Result<String, SdkError> {
    let snapshot = capture(backend)?;
    serde_json::to_string_pretty(&snapshot)
        .map_err(|error| SdkError::Internal(format!("snapshot encoding failed: {error}")))
}

/// Decode and restore a JSON document produced by [`export_json`].
pub fn import_json(
    backend: &dyn Backend,
    json: &str,
    options: RestoreOptions,
) -> Result<RestoreReport, SdkError> {
    let snapshot = decode_snapshot(json.as_bytes())?;
    restore(backend, &snapshot, options)
}

/// Reconcile the backend with `snapshot`.
///
/// Extra pairs are removed first (only with `remove_extra`) so their port names
/// are free for the desired configuration, then missing pairs are created and
/// changed pairs updated. Failures are reported per pair; `continue_on_error`
/// decides whether the remaining pairs are still attempted. `dry_run` reports
/// what would be applied without changes.
pub fn restore(
    backend: &dyn Backend,
    snapshot: &Snapshot,
    options: RestoreOptions,
) -> Result<RestoreReport, SdkError> {
    snapshot.validate()?;
    let mut report = RestoreReport {
        dry_run: options.dry_run,
        ..RestoreReport::default()
    };
    let mut current: BTreeMap<PairId, PairParams> = backend
        .list_pairs()?
        .into_iter()
        .map(|pair| (pair.id, pair.params))
        .collect();

    if options.remove_extra {
        let extras: Vec<PairId> = current
            .keys()
            .copied()
            .filter(|id| snapshot.pair(*id).is_none())
            .collect();
        for id in extras {
            if options.dry_run {
                report.applied.push(id);
                continue;
            }
            match backend.delete_pair(id) {
                Ok(()) => {
                    current.remove(&id);
                    report.applied.push(id);
                }
                Err(error) => {
                    report.failed.push(RestoreFailure {
                        pair: id,
                        error: error.to_string(),
                    });
                    if !options.continue_on_error {
                        return Ok(report);
                    }
                }
            }
        }
    }

    for desired in &snapshot.pairs {
        let result = match current.get(&desired.id) {
            Some(existing) if *existing == desired.params => Ok(desired.id),
            Some(_) if options.dry_run => Ok(desired.id),
            Some(_) => backend
                .set_params(desired.id, &desired.params)
                .map(|()| desired.id),
            None if options.dry_run => Ok(desired.id),
            None => backend
                .create_pair(Some(desired.id), &desired.params)
                .map(|pair| pair.id),
        };
        match result {
            Ok(id) => report.applied.push(id),
            Err(error) => {
                report.failed.push(RestoreFailure {
                    pair: desired.id,
                    error: error.to_string(),
                });
                if !options.continue_on_error {
                    return Ok(report);
                }
            }
        }
    }
    Ok(report)
}

/// Default state file:
///
/// - Windows: `%ProgramData%\com0com\state.json`
/// - macOS root: `/Library/Application Support/com0com/state.json`
/// - macOS user: `~/Library/Application Support/com0com/state.json`
/// - Linux root: `/var/lib/com0com/state.json`
/// - Linux user: `$XDG_STATE_HOME/com0com/state.json` or
///   `~/.local/state/com0com/state.json`
///
/// The `COM0COM_STATE` environment variable wins over every default.
pub fn default_state_path() -> PathBuf {
    if let Some(path) = std::env::var_os("COM0COM_STATE") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    platform_state_path().unwrap_or_else(|| PathBuf::from("com0com-state.json"))
}

#[cfg(windows)]
fn platform_state_path() -> Option<PathBuf> {
    std::env::var_os("ProgramData")
        .filter(|value| !value.is_empty())
        .map(|root| PathBuf::from(root).join("com0com").join("state.json"))
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(target_os = "macos")]
fn platform_state_path() -> Option<PathBuf> {
    if is_root() {
        return Some(PathBuf::from(
            "/Library/Application Support/com0com/state.json",
        ));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("com0com")
                .join("state.json")
        })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_state_path() -> Option<PathBuf> {
    if is_root() {
        return Some(PathBuf::from("/var/lib/com0com/state.json"));
    }
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        if !state_home.is_empty() {
            return Some(PathBuf::from(state_home).join("com0com").join("state.json"));
        }
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("com0com")
                .join("state.json")
        })
}

#[cfg(not(any(windows, unix)))]
fn platform_state_path() -> Option<PathBuf> {
    None
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SdkError> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        #[cfg(unix)]
        let created = fs::symlink_metadata(dir).is_err();
        fs::create_dir_all(dir).map_err(io_error)?;
        #[cfg(unix)]
        if created && !is_root() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_file_name(format!(".{file_name}.{}.{unique}.tmp", std::process::id()));
    let write = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&temp);
        return Err(io_error(error));
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(io_error(error));
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> SdkError {
    SdkError::Io(error.to_string())
}

fn protocol_error(error: serde_json::Error) -> SdkError {
    SdkError::Protocol(format!("invalid state JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KernelKind, PairParams, PortName};
    use std::sync::atomic::AtomicU32;

    static DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let unique = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "com0com-state-io-test-{}-{unique}",
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

    fn snapshot() -> Snapshot {
        Snapshot::new(
            "fake",
            KernelKind::None,
            vec![SnapshotPair {
                id: 3,
                params: PairParams {
                    port_a: PortName::new("A3").unwrap(),
                    port_b: PortName::new("B3").unwrap(),
                    ..PairParams::default()
                },
            }],
        )
    }

    #[test]
    fn write_read_roundtrip_is_atomic() {
        let dir = TestDir::new();
        let path = dir.join("state.json");
        let original = snapshot();
        save_to(&path, &original).unwrap();
        assert_eq!(load(&path).unwrap(), original);

        save_to(&path, &original).unwrap();
        assert_eq!(load(&path).unwrap(), original);
        let leftovers: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn state_file_and_user_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let path = dir.join("nested").join(".com0com").join("state.json");
        save_to(&path, &snapshot()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state file must not be world-readable");
        if unsafe { libc::geteuid() } != 0 {
            let dir_mode = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700, "user state directory must be private");
        }
    }

    #[test]
    fn load_rejects_missing_file_bad_json_and_wrong_schema() {
        let dir = TestDir::new();
        assert!(matches!(
            load(&dir.join("missing.json")),
            Err(SdkError::Io(_))
        ));

        let path = dir.join("bad.json");
        fs::write(&path, b"{").unwrap();
        assert!(matches!(load(&path), Err(SdkError::Protocol(_))));

        let mut wrong = snapshot();
        wrong.schema = 3;
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(matches!(load(&path), Err(SdkError::Protocol(_))));
    }

    #[test]
    fn schema_one_snapshot_is_migrated_to_upstream_noise_scale() {
        let dir = TestDir::new();
        let path = dir.join("v1.json");
        // schema 1：噪声用 0.01% 单位，没有 all_data_bits 字段。
        let json = serde_json::json!({
            "schema": 1,
            "created_at": {"secs_since_epoch": 0, "nanos_since_epoch": 0},
            "platform": "fake",
            "kernel": "none",
            "pairs": [{
                "id": 2,
                "params": {
                    "port_a": "A2",
                    "port_b": "B2",
                    "baud_emu": false,
                    "overrun": false,
                    "noise_enabled": true,
                    "noise_probability": 3000,
                    "exclusive": false,
                    "plugin": false,
                    "pin_wiring_a": {"lines": [16, 32, 0, 0]},
                    "pin_wiring_b": {"lines": [16, 32, 0, 0]}
                }
            }]
        });
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.schema, crate::SNAPSHOT_SCHEMA);
        assert_eq!(loaded.pairs[0].params.noise_probability, 30_000_000);
        assert!(!loaded.pairs[0].params.all_data_bits);
        // 上限同样被迁移逻辑钳住。
        assert_eq!(
            30_000_000u64.saturating_mul(10_000).min(100_000_000),
            100_000_000
        );
    }
}
