//! Unified error type returned by every backend.

use std::fmt;

/// Stable error category used by the C ABI and JSON-RPC mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Unsupported,
    PermissionDenied,
    DriverUnavailable,
    NotFound,
    AlreadyExists,
    InvalidArgument,
    Busy,
    Io,
    Win32,
    Timeout,
    DaemonUnavailable,
    Protocol,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdkError {
    Unsupported,
    PermissionDenied,
    DriverUnavailable,
    NotFound,
    AlreadyExists,
    InvalidArgument(String),
    Busy,
    Io(String),
    Win32 {
        op: String,
        code: u32,
        hint: Option<String>,
    },
    Timeout,
    DaemonUnavailable,
    Protocol(String),
    Internal(String),
}

impl SdkError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Unsupported => ErrorKind::Unsupported,
            Self::PermissionDenied => ErrorKind::PermissionDenied,
            Self::DriverUnavailable => ErrorKind::DriverUnavailable,
            Self::NotFound => ErrorKind::NotFound,
            Self::AlreadyExists => ErrorKind::AlreadyExists,
            Self::InvalidArgument(_) => ErrorKind::InvalidArgument,
            Self::Busy => ErrorKind::Busy,
            Self::Io(_) => ErrorKind::Io,
            Self::Win32 { .. } => ErrorKind::Win32,
            Self::Timeout => ErrorKind::Timeout,
            Self::DaemonUnavailable => ErrorKind::DaemonUnavailable,
            Self::Protocol(_) => ErrorKind::Protocol,
            Self::Internal(_) => ErrorKind::Internal,
        }
    }

    pub fn win32_code(&self) -> Option<u32> {
        match self {
            Self::Win32 { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl fmt::Display for SdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("operation is not supported by this backend"),
            Self::PermissionDenied => f.write_str("permission denied"),
            Self::DriverUnavailable => f.write_str("com0com driver is not available"),
            Self::NotFound => f.write_str("pair not found"),
            Self::AlreadyExists => f.write_str("pair already exists"),
            Self::InvalidArgument(msg) => write!(f, "invalid argument: {msg}"),
            Self::Busy => f.write_str("resource is busy"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
            Self::Win32 { op, code, hint } => {
                write!(f, "{op} failed (Win32 {code})")?;
                if let Some(hint) = hint {
                    write!(f, ": {hint}")?;
                }
                Ok(())
            }
            Self::Timeout => f.write_str("operation timed out"),
            Self::DaemonUnavailable => f.write_str("com0com daemon is not available"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for SdkError {}

impl From<std::io::Error> for SdkError {
    fn from(value: std::io::Error) -> Self {
        match value.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Self::Timeout,
            _ => Self::Io(value.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_codes_are_stable() {
        assert_eq!(SdkError::Unsupported.kind(), ErrorKind::Unsupported);
        let err = SdkError::Win32 {
            op: "DeviceIoControl".into(),
            code: 5,
            hint: Some("run as administrator".into()),
        };
        assert_eq!(err.kind(), ErrorKind::Win32);
        assert_eq!(err.win32_code(), Some(5));
        assert_eq!(
            err.to_string(),
            "DeviceIoControl failed (Win32 5): run as administrator"
        );
    }

    #[test]
    fn io_errors_map_to_categories() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(SdkError::from(denied), SdkError::PermissionDenied);
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(SdkError::from(missing), SdkError::NotFound);
    }
}
