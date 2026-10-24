//! Win32 error-code to [`SdkError`] mapping.
//!
//! The bus driver returns NTSTATUS values, which the I/O manager exposes to
//! user mode as Win32 codes. Mapping happens in one place so setupc and the UI
//! see the same categories. `ERROR_INVALID_PARAMETER` is deliberately left as
//! [`SdkError::Win32`] here: the driver uses it for "no such pair", and the bus
//! client translates it per operation.

#[cfg(any(windows, test))]
use com0com_core::SdkError;

#[cfg(windows)]
pub(crate) const ERROR_INVALID_HANDLE: u32 = 6;

#[cfg(any(windows, test))]
pub(crate) fn map_ioctl(op: &'static str, code: u32) -> SdkError {
    match code {
        // ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD
        5 | 1314 => SdkError::PermissionDenied,
        // ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND |
        // ERROR_DEVICE_NOT_CONNECTED | ERROR_NOT_FOUND | ERROR_NO_SUCH_DEVICE
        2 | 3 | 1167 | 1168 | 433 => SdkError::NotFound,
        // ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS
        80 | 183 => SdkError::AlreadyExists,
        // ERROR_BUSY | ERROR_DEVICE_IN_USE
        170 | 2404 => SdkError::Busy,
        // ERROR_NOT_ENOUGH_MEMORY | ERROR_OUTOFMEMORY
        8 | 14 => SdkError::Io(format!("driver ran out of memory (Win32 {code})")),
        // ERROR_INSUFFICIENT_BUFFER: the driver rejected our fixed-size buffer.
        122 => SdkError::Internal("driver reported an undersized IOCTL buffer".into()),
        _ => SdkError::Win32 {
            op: op.to_string(),
            code,
            hint: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_win32_codes() {
        assert_eq!(map_ioctl("X", 5), SdkError::PermissionDenied);
        assert_eq!(map_ioctl("X", 2), SdkError::NotFound);
        assert_eq!(map_ioctl("X", 183), SdkError::AlreadyExists);
        assert_eq!(map_ioctl("X", 170), SdkError::Busy);
        // ERROR_INVALID_PARAMETER stays a Win32 error for the caller to refine.
        assert_eq!(map_ioctl("GET_PARAMS", 87).win32_code(), Some(87));
        assert_eq!(
            map_ioctl("GET_PARAMS", 87).to_string(),
            "GET_PARAMS failed (Win32 87)"
        );
    }
}
