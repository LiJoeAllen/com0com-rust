//! Local JSON-RPC transport shared by the daemon and its clients.
//!
//! The wire format is newline-delimited JSON-RPC 2.0, capped at
//! [`MAX_FRAME_SIZE`] bytes per frame. The first frame on every connection
//! must be `rpc.hello` carrying [`PROTOCOL_VERSION`], so version skew fails
//! fast with a clear error instead of misdispatched methods.
//!
//! Windows uses a named pipe with a DACL limited to SYSTEM, Administrators
//! and the current user; Unix uses a `0600` socket. Nothing listens on the
//! network.

pub use com0com_core::PROTOCOL_VERSION;

pub mod framing;
pub mod protocol;
pub mod remote;
pub mod service;
pub mod transport;

pub use framing::{Connection, FrameReader, FrameWriter, SharedWriter};
pub use protocol::{
    CreatePairParams, DaemonStatusResult, EventParams, HelloParams, HelloResult, LogsParams,
    Notification, OpenSerialParams, PairIdParams, Request, Response, RpcError, SerialHandleParams,
    SubscribeParams, UnsubscribeParams, CODE_ALREADY_EXISTS, CODE_BUSY, CODE_DAEMON_UNAVAILABLE,
    CODE_DRIVER_UNAVAILABLE, CODE_INTERNAL, CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_IO,
    CODE_METHOD_NOT_FOUND, CODE_NOT_FOUND, CODE_PARSE, CODE_PERMISSION, CODE_PROTOCOL,
    CODE_TIMEOUT, CODE_UNSUPPORTED, METHOD_CAPABILITIES, METHOD_DAEMON_LOGS, METHOD_DAEMON_STATUS,
    METHOD_DRIVER_INFO, METHOD_DRIVER_LOGS, METHOD_EVENTS_SUBSCRIBE, METHOD_EVENTS_UNSUBSCRIBE,
    METHOD_HELLO, METHOD_PAIRS_CREATE, METHOD_PAIRS_DELETE, METHOD_PAIRS_GET, METHOD_PAIRS_LIST,
    METHOD_PAIRS_RESET_PARAMS, METHOD_PAIRS_SET_PARAMS, METHOD_PING, METHOD_PORTS_LIST,
    METHOD_SERIAL_CLOSE, METHOD_SERIAL_OPEN, METHOD_SERIAL_WRITE, METHOD_SHUTDOWN,
    METHOD_STATE_EXPORT, METHOD_STATE_IMPORT, METHOD_STATE_LOAD, METHOD_STATE_RESTORE,
    METHOD_STATE_SAVE, METHOD_STATE_SNAPSHOT, NOTIFY_EVENT, NOTIFY_SERIAL_CLOSED,
    NOTIFY_SERIAL_DATA,
};
pub use remote::{Client, RemoteBackend};
pub use service::{serve_connection, DaemonLog, ServerContext};
pub use transport::{connect, memory_pair, Listener};

/// Windows named pipe used by `com0comd`.
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\com0comd-v1";

/// Hard limit for one newline-delimited JSON frame.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// Linux control socket.
#[cfg(target_os = "linux")]
pub const DEFAULT_SOCKET_PATH: &str = "/run/com0com/control.sock";

/// macOS control socket.
#[cfg(target_os = "macos")]
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/com0com/control.sock";

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/com0com/control.sock";

/// Default local endpoint for the current platform.
///
/// A root daemon uses the system socket path; an unprivileged caller falls
/// back to a user-scoped directory so no elevation is required. The
/// `COM0COM_ENDPOINT` environment variable overrides both.
pub fn default_endpoint() -> String {
    #[cfg(windows)]
    {
        DEFAULT_PIPE_NAME.to_string()
    }
    #[cfg(unix)]
    {
        if let Some(path) = std::env::var_os("COM0COM_ENDPOINT").filter(|value| !value.is_empty()) {
            return path.to_string_lossy().into_owned();
        }
        if is_root() {
            return DEFAULT_SOCKET_PATH.to_string();
        }
        // A running root daemon owns the system socket; unprivileged clients
        // must still find it (socket mode 0660 + peer-cred check decide access).
        if std::path::Path::new(DEFAULT_SOCKET_PATH).exists() {
            return DEFAULT_SOCKET_PATH.to_string();
        }
        user_socket_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_string())
    }
    #[cfg(not(any(windows, unix)))]
    {
        "com0comd".to_string()
    }
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(target_os = "linux")]
fn user_socket_path() -> Option<std::path::PathBuf> {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        return Some(std::path::PathBuf::from(runtime).join("com0com/control.sock"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| std::path::PathBuf::from(home).join(".local/state/com0com/control.sock"))
}

#[cfg(target_os = "macos")]
fn user_socket_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            std::path::PathBuf::from(home).join("Library/Application Support/com0com/control.sock")
        })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn user_socket_path() -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_limit_is_one_mib() {
        assert_eq!(MAX_FRAME_SIZE, 1024 * 1024);
        assert_eq!(PROTOCOL_VERSION, 1);
        assert!(!default_endpoint().is_empty());
    }
}
