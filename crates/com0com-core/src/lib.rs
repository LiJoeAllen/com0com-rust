//! Platform-independent contract shared by every com0com backend.
//!
//! This crate must not depend on `windows-sys`, `libc` or WDK crates: it is the
//! single place where the SDK, the C ABI, the JSON-RPC daemon and the platform
//! backends agree on semantics.

pub mod backend;
pub mod capabilities;
pub mod error;
pub mod event;
#[cfg(feature = "fake")]
pub mod fake;
pub mod model;
pub mod state;
pub mod state_io;

pub use backend::{
    Backend, BackendInfo, DriverInfo, DriverKind, DriverStatus, KernelKind, LogEntry, LogLevel,
    MonitorHandle,
};
pub use capabilities::Capabilities;
pub use error::{ErrorKind, SdkError};
pub use event::{Event, EventFilter, EventKind, EventSubscription, MonitorEvent, RecvError};
pub use model::{
    FlowControl, Pair, PairId, PairParams, Parity, PinWiring, PortConfig, PortIndex, PortInfo,
    PortName, StopBits, MAX_PAIRS, MAX_PAIR_ID, MAX_PORT_NAME_CHARS, PAIR_COUNT,
};
pub use state::{
    RestoreFailure, RestoreOptions, RestoreReport, Snapshot, SnapshotPair, SNAPSHOT_SCHEMA,
};
pub use state_io::default_state_path;

/// JSON-RPC / SDK protocol version implemented by this crate.
pub const PROTOCOL_VERSION: u32 = 1;
