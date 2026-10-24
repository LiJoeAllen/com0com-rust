//! Stable, synchronous Rust SDK facade.
//!
//! UI code depends on this crate only. The platform backend is selected here:
//! Windows uses the kernel-driver backend, Unix uses the PTY backend, and any
//! other platform gets an explicit `Unsupported` backend. A daemon backend can
//! be selected with [`Sdk::connect_daemon`] or [`SdkBuilder`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use com0com_core::EventSubscription;
/// Contract types re-exported so UI code only needs this crate.
pub use com0com_core::{
    Backend, BackendInfo, Capabilities, DriverInfo, DriverKind, DriverStatus, Event, EventFilter,
    EventKind, KernelKind, LogEntry, LogLevel, MonitorEvent, Pair, PairId, PairParams, PortConfig,
    PortInfo, PortName, RecvError, RestoreOptions, RestoreReport, SdkError, Snapshot, MAX_PAIRS,
    MAX_PAIR_ID,
};

pub mod state;

pub use state::{default_state_path, StateApi};

#[cfg(feature = "fake")]
pub use com0com_core::fake::FakeBackend;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// In-process kernel/PTY backend for the current platform.
    Native,
    /// Local JSON-RPC daemon (named pipe / Unix socket).
    Daemon,
}

pub struct Sdk {
    backend: Arc<dyn Backend>,
    /// Present when `backend` is a daemon connection, so state calls can be
    /// routed to the daemon that owns the devices.
    remote: Option<Arc<com0com_ipc::RemoteBackend>>,
}

impl Sdk {
    /// Open the natural backend for this platform.
    ///
    /// Unix prefers a running `com0comd` (the daemon owns the PTYs, so a
    /// short-lived CLI cannot create them itself) and falls back to an
    /// in-process PTY manager when no daemon is reachable. Windows always uses
    /// the kernel-driver backend.
    pub fn open() -> Result<Self, SdkError> {
        #[cfg(unix)]
        {
            let endpoint = std::env::var("COM0COM_ENDPOINT").ok();
            match Self::connect_daemon_timeout(endpoint.as_deref(), Duration::from_millis(250)) {
                Ok(sdk) => Ok(sdk),
                Err(SdkError::DaemonUnavailable) => Self::open_native(),
                Err(error) => Err(error),
            }
        }
        #[cfg(not(unix))]
        {
            Self::open_native()
        }
    }

    /// Open the in-process backend for this platform, ignoring any daemon.
    pub fn open_native() -> Result<Self, SdkError> {
        #[cfg(windows)]
        {
            Ok(Self {
                backend: Arc::new(com0com_backend_win::WinBackend::open()?),
                remote: None,
            })
        }
        #[cfg(all(unix, not(windows)))]
        {
            Ok(Self {
                backend: Arc::new(com0com_backend_pty::PtyBackend::open()?),
                remote: None,
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            Ok(Self {
                backend: Arc::new(com0com_backend_stub::StubBackend::new(std::env::consts::OS)),
                remote: None,
            })
        }
    }

    /// Wrap a caller-provided backend (used by tests and the daemon).
    pub fn with_backend(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            remote: None,
        }
    }

    pub fn builder() -> SdkBuilder {
        SdkBuilder::default()
    }

    /// Connect to a local daemon over its named pipe / Unix socket.
    pub fn connect_daemon(endpoint: Option<&str>) -> Result<Self, SdkError> {
        Self::connect_daemon_timeout(endpoint, Duration::from_secs(2))
    }

    fn connect_daemon_timeout(endpoint: Option<&str>, timeout: Duration) -> Result<Self, SdkError> {
        let remote = Arc::new(com0com_ipc::RemoteBackend::connect_with_timeout(
            endpoint, timeout,
        )?);
        Ok(Self {
            backend: Arc::clone(&remote) as Arc<dyn Backend>,
            remote: Some(remote),
        })
    }

    pub fn info(&self) -> BackendInfo {
        self.backend.info()
    }

    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    pub fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        self.backend.list_pairs()
    }

    pub fn create_pair(&self, id: Option<PairId>, params: &PairParams) -> Result<Pair, SdkError> {
        self.backend.create_pair(id, params)
    }

    pub fn delete_pair(&self, id: PairId) -> Result<(), SdkError> {
        self.backend.delete_pair(id)
    }

    pub fn get_params(&self, id: PairId) -> Result<PairParams, SdkError> {
        self.backend.get_params(id)
    }

    pub fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError> {
        self.backend.set_params(id, params)
    }

    pub fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
        self.backend.list_ports()
    }

    pub fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError> {
        self.backend.subscribe(filter)
    }

    pub fn open_monitor(&self, port: &PortName, cfg: &PortConfig) -> Result<Monitor, SdkError> {
        let handle = self.backend.open_monitor(port, cfg)?;
        Ok(Monitor { handle })
    }

    pub fn driver_info(&self, kind: DriverKind) -> Result<DriverInfo, SdkError> {
        self.backend.driver_info(kind)
    }

    pub fn driver_logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        self.backend.driver_logs(limit)
    }

    /// State API bound to the platform default state file, or to the daemon's
    /// configured state file when this SDK is connected over IPC.
    pub fn state(&self) -> StateApi {
        if let Some(remote) = &self.remote {
            let path = remote
                .status()
                .map(|status| PathBuf::from(status.state_path))
                .unwrap_or_else(|_| PathBuf::from("<com0comd>"));
            return StateApi::remote(Arc::clone(remote), path);
        }
        StateApi::new(Arc::clone(&self.backend), default_state_path())
    }

    /// State API bound to an explicit local file (daemons and tests). The file
    /// is always written by this process, even for a daemon backend.
    pub fn state_at(&self, path: impl Into<PathBuf>) -> StateApi {
        StateApi::new(Arc::clone(&self.backend), path)
    }

    /// Daemon status when connected over IPC.
    pub fn daemon_status(&self) -> Option<Result<com0com_ipc::DaemonStatusResult, SdkError>> {
        self.remote.as_ref().map(|remote| remote.status())
    }
}

#[derive(Default)]
pub struct SdkBuilder {
    kind: Option<BackendKind>,
    endpoint: Option<String>,
}

impl SdkBuilder {
    pub fn backend(mut self, kind: BackendKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn open(self) -> Result<Sdk, SdkError> {
        match self.kind.unwrap_or(BackendKind::Native) {
            BackendKind::Native => Sdk::open(),
            BackendKind::Daemon => Sdk::connect_daemon(self.endpoint.as_deref()),
        }
    }
}

/// A live serial monitor. Reads arrive through a bounded queue; the producer
/// counts discarded chunks in [`Monitor::dropped`].
pub struct Monitor {
    handle: Box<dyn com0com_core::MonitorHandle>,
}

impl Monitor {
    pub fn write(&mut self, data: &[u8]) -> Result<usize, SdkError> {
        self.handle.write(data)
    }

    pub fn events(&self) -> &std::sync::mpsc::Receiver<MonitorEvent> {
        self.handle.events()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<MonitorEvent, RecvError> {
        self.handle.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<MonitorEvent, RecvError> {
        self.handle.try_recv()
    }

    pub fn dropped(&self) -> u64 {
        self.handle.dropped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use com0com_core::fake::FakeBackend;
    use com0com_core::{Event, EventKind, RecvError};

    fn sdk_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Sdk>();
    }

    #[test]
    fn send_sync_contract() {
        sdk_is_send_sync();
    }

    #[test]
    fn facade_delegates_to_backend() {
        let sdk = Sdk::with_backend(Arc::new(FakeBackend::new()));
        assert_eq!(sdk.list_pairs().unwrap().len(), 0);
        let pair = sdk.create_pair(None, &PairParams::default()).unwrap();
        assert_eq!(pair.id, 0);
        assert_eq!(sdk.list_ports().unwrap().len(), 2);
        sdk.delete_pair(0).unwrap();
        assert!(sdk.list_pairs().unwrap().is_empty());
    }

    #[test]
    fn subscriptions_coexist_with_independent_filters() {
        let sdk = Sdk::with_backend(Arc::new(FakeBackend::new()));
        let all = sdk.subscribe(EventFilter::all()).unwrap();
        let removes = sdk
            .subscribe(EventFilter::kinds(&[EventKind::PairRemoved]))
            .unwrap();

        let pair = sdk.create_pair(None, &PairParams::default()).unwrap();
        match all.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::PairAdded { pair: seen } => assert_eq!(seen, pair),
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(removes.try_recv(), Err(RecvError::Empty));

        sdk.delete_pair(pair.id).unwrap();
        assert_eq!(
            all.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::PairRemoved { id: pair.id }
        );
        assert_eq!(
            removes.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::PairRemoved { id: pair.id }
        );
        assert_eq!(all.dropped(), 0);
        assert_eq!(removes.dropped(), 0);
    }
}
