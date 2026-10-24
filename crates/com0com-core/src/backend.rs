//! The platform-independent backend contract.

use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    Capabilities, Event, EventFilter, EventSubscription, MonitorEvent, Pair, PairId, PairParams,
    PortConfig, PortInfo, PortName, RecvError, SdkError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelKind {
    /// Existing `com0combus` + `com0comport` KMDF/WDM pair.
    WindowsKmdfWdm,
    /// Linux/macOS user-mode PTY bridge.
    UnixPty,
    None,
}

impl KernelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsKmdfWdm => "windows-kmdf-wdm",
            Self::UnixPty => "unix-pty",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub platform: String,
    pub kernel: KernelKind,
    pub kernel_version: Option<String>,
    pub protocol_version: u32,
}

impl BackendInfo {
    pub fn new(
        platform: impl Into<String>,
        kernel: KernelKind,
        kernel_version: Option<String>,
    ) -> Self {
        Self {
            platform: platform.into(),
            kernel,
            kernel_version,
            protocol_version: crate::PROTOCOL_VERSION,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Bus,
    Port,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverStatus {
    Running,
    Stopped,
    StartPending,
    StopPending,
    NotInstalled,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverInfo {
    pub kind: DriverKind,
    pub status: DriverStatus,
    pub service_name: String,
    pub image_path: Option<String>,
    pub version: Option<String>,
    pub sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warning,
    Information,
    Verbose,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

/// A live serial monitor. Implementations own a reader thread; the receiver is
/// bounded so a slow consumer applies backpressure instead of losing data
/// silently (drops are counted by [`MonitorHandle::dropped`]).
pub trait MonitorHandle: Send {
    fn write(&mut self, data: &[u8]) -> Result<usize, SdkError>;
    fn events(&self) -> &mpsc::Receiver<MonitorEvent>;

    fn recv_timeout(&self, timeout: Duration) -> Result<MonitorEvent, RecvError> {
        match self.events().recv_timeout(timeout) {
            Ok(event) => Ok(event),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RecvError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(RecvError::Disconnected),
        }
    }

    fn try_recv(&self) -> Result<MonitorEvent, RecvError> {
        match self.events().try_recv() {
            Ok(event) => Ok(event),
            Err(mpsc::TryRecvError::Empty) => Err(RecvError::Empty),
            Err(mpsc::TryRecvError::Disconnected) => Err(RecvError::Disconnected),
        }
    }

    fn dropped(&self) -> u64;
}

/// Platform contract. One implementation exists per supported kernel/transport.
pub trait Backend: Send + Sync {
    fn info(&self) -> BackendInfo;
    fn capabilities(&self) -> Capabilities;

    fn list_pairs(&self) -> Result<Vec<Pair>, SdkError>;
    fn create_pair(&self, id: Option<PairId>, params: &PairParams) -> Result<Pair, SdkError>;
    fn delete_pair(&self, id: PairId) -> Result<(), SdkError>;
    fn get_params(&self, id: PairId) -> Result<PairParams, SdkError>;
    fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError>;

    fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError>;

    fn open_monitor(
        &self,
        port: &PortName,
        cfg: &PortConfig,
    ) -> Result<Box<dyn MonitorHandle>, SdkError>;

    fn driver_info(&self, kind: DriverKind) -> Result<DriverInfo, SdkError>;
    fn driver_logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError>;

    fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError>;
}

/// Convenience alias for the event type re-exported from `event`.
pub type BackendEvent = Event;
