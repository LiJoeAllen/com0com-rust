//! Byte-level transport to the bus control device.
//!
//! The [`BusTransport`] trait is the only seam between the backend logic and
//! Win32: production code uses [`DeviceTransport`] (SetupAPI + DeviceIoControl),
//! tests inject an in-memory fake.

use com0com_core::SdkError;

/// Issue one `DeviceIoControl`-style request.
///
/// `op` is the setupc-compatible operation label (`ENUM_PAIRS`, `CREATE_PAIR`,
/// ...) used in error messages. Implementations return the number of bytes
/// written to `output`.
pub trait BusTransport: Send + Sync {
    fn ioctl(
        &self,
        op: &'static str,
        code: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, SdkError>;
}

/// Zero-copy view of a `#[repr(C)]` `Copy` struct as bytes.
pub(crate) fn struct_bytes<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: `v` is a live `#[repr(C)]` value; the slice is immutable and
    // covers exactly `size_of::<T>()` initialised bytes.
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

/// Copy a `#[repr(C)]` `Copy` struct out of a raw IOCTL buffer.
pub(crate) fn struct_of<T: Copy>(buf: &[u8]) -> T {
    assert!(
        buf.len() >= std::mem::size_of::<T>(),
        "IOCTL buffer too small for target struct"
    );
    let mut v = std::mem::MaybeUninit::<T>::uninit();
    // SAFETY: the destination is sized for `T` and the source covers
    // `size_of::<T>()` bytes; `T: Copy` makes the bit copy valid.
    unsafe {
        std::ptr::copy_nonoverlapping(
            buf.as_ptr(),
            v.as_mut_ptr() as *mut u8,
            std::mem::size_of::<T>(),
        );
        v.assume_init()
    }
}

#[cfg(windows)]
pub(crate) use device::guid_from_bytes;
#[cfg(windows)]
pub use device::DeviceTransport;

#[cfg(windows)]
mod device {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use std::sync::Mutex;

    use windows_sys::core::GUID;
    use windows_sys::Win32::errhandlingapi::GetLastError;
    use windows_sys::Win32::fileapi::{CreateFileW, OPEN_EXISTING};
    use windows_sys::Win32::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::ioapiset::DeviceIoControl;
    use windows_sys::Win32::minwinbase::OVERLAPPED;
    use windows_sys::Win32::setupapi::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
        SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    };
    use windows_sys::Win32::winerror::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::winnt::{FILE_ATTRIBUTE_NORMAL, GENERIC_READ, GENERIC_WRITE, HANDLE};

    use com0com_common::constants::GUID_COM0COM_BUS_BYTES;
    use com0com_core::SdkError;

    use super::BusTransport;
    use crate::errors::map_ioctl;

    /// Lazily-opened handle to the bus control device.
    ///
    /// The handle is stored as `isize` so the struct is `Send + Sync` on every
    /// host (raw Win32 handles are pointers and do not implement those traits).
    pub struct DeviceTransport {
        handle: Mutex<Option<isize>>,
    }

    impl DeviceTransport {
        pub fn new() -> Self {
            Self {
                handle: Mutex::new(None),
            }
        }
    }

    impl Default for DeviceTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for DeviceTransport {
        fn drop(&mut self) {
            if let Some(raw) = self.handle.get_mut().ok().and_then(Option::take) {
                // SAFETY: the value came from `CreateFileW` and is owned here.
                unsafe { CloseHandle(raw as usize as HANDLE) };
            }
        }
    }

    impl BusTransport for DeviceTransport {
        fn ioctl(
            &self,
            op: &'static str,
            code: u32,
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, SdkError> {
            let mut guard = self.handle.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_none() {
                *guard = Some(open_bus()? as usize as isize);
            }
            let handle = guard.expect("bus handle was opened above") as usize as HANDLE;
            match raw_ioctl(handle, code, input, output) {
                Ok(bytes) => Ok(bytes),
                Err(win32) => {
                    // A stale handle after a driver reload must not poison the
                    // backend forever: drop it so the next call re-enumerates.
                    if win32 == crate::errors::ERROR_INVALID_HANDLE {
                        // SAFETY: handle ownership stays with this struct.
                        unsafe { CloseHandle(handle) };
                        *guard = None;
                    }
                    Err(map_ioctl(op, win32))
                }
            }
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    /// Build a `windows_sys::core::GUID` from little-endian GUID bytes.
    pub(crate) fn guid_from_bytes(b: &[u8; 16]) -> GUID {
        let data1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let data2 = u16::from_le_bytes([b[4], b[5]]);
        let data3 = u16::from_le_bytes([b[6], b[7]]);
        let mut data4 = [0u8; 8];
        data4.copy_from_slice(&b[8..16]);
        GUID {
            data1,
            data2,
            data3,
            data4,
        }
    }

    struct DeviceInfoList(HDEVINFO);

    impl Drop for DeviceInfoList {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `SetupDiGetClassDevsW` and is non-null
            // (checked before construction).
            unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    /// Device path of the first present `GUID_COM0COM_BUS` interface.
    fn bus_device_path() -> Result<String, SdkError> {
        let guid = guid_from_bytes(&GUID_COM0COM_BUS_BYTES);
        // SAFETY: plain SetupAPI call with a null parent window and no
        // insertion flags pointer.
        let hdev = unsafe {
            SetupDiGetClassDevsW(
                &guid,
                null(),
                null_mut(),
                (DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) as u32,
            )
        };
        if hdev == INVALID_HANDLE_VALUE {
            return Err(map_open("SetupDiGetClassDevsW", unsafe { GetLastError() }));
        }
        let hdev = DeviceInfoList(hdev);

        let mut didata: SP_DEVICE_INTERFACE_DATA = unsafe { zeroed() };
        didata.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

        // SAFETY: `hdev.0` is valid and `didata` is fully initialised.
        let found = unsafe { SetupDiEnumDeviceInterfaces(hdev.0, null(), &guid, 0, &mut didata) };
        if found == 0 {
            let code = unsafe { GetLastError() };
            return Err(match code {
                // ERROR_FILE_NOT_FOUND | ERROR_NOT_FOUND | ERROR_NO_MORE_ITEMS
                2 | 1168 | 259 => SdkError::DriverUnavailable,
                5 => SdkError::PermissionDenied,
                _ => map_open("SetupDiEnumDeviceInterfaces", code),
            });
        }

        let mut required: u32 = 0;
        // SAFETY: size probe; a null detail pointer is explicitly supported.
        let probe_ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                hdev.0,
                &didata,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            )
        };
        let probe_err = unsafe { GetLastError() };
        if probe_ok != 0 || probe_err != ERROR_INSUFFICIENT_BUFFER as u32 || required == 0 {
            return Err(map_open("SetupDiGetDeviceInterfaceDetailW", probe_err));
        }

        // Allocate in `usize` units so the DETAIL_DATA alignment requirement is
        // satisfied; `required` includes the variable-length path.
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut buf: Vec<usize> = vec![0; words];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        // Documented fixed part size: 8 on x64 (with padding), 6 on x86.
        let detail_cb_size: u32 = if size_of::<usize>() == 8 { 8 } else { 6 };
        // SAFETY: the allocation is large enough for the detail struct.
        unsafe {
            (*detail).cbSize = detail_cb_size;
        }
        // SAFETY: buffer length and detail size match the probe result.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                hdev.0,
                &didata,
                detail,
                required,
                null_mut(),
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(map_open("SetupDiGetDeviceInterfaceDetailW", unsafe {
                GetLastError()
            }));
        }

        let path = unsafe {
            let path_ptr = &(*detail).DevicePath as *const u16;
            let mut len = 0usize;
            while *path_ptr.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(path_ptr, len))
        };
        Ok(path)
    }

    fn open_bus() -> Result<HANDLE, SdkError> {
        let path = bus_device_path()?;
        let wide = to_wide(&path);
        // SAFETY: NUL-terminated path, no security attributes, no template.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE as u32,
                0,
                null(),
                OPEN_EXISTING as u32,
                FILE_ATTRIBUTE_NORMAL as u32,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            return Err(match code {
                5 | 1314 => SdkError::PermissionDenied,
                2 | 3 => SdkError::DriverUnavailable,
                _ => SdkError::Win32 {
                    op: "CreateFileW".into(),
                    code,
                    hint: Some("Try running as Administrator.".into()),
                },
            });
        }
        Ok(handle)
    }

    fn map_open(op: &'static str, code: u32) -> SdkError {
        match code {
            5 | 1314 => SdkError::PermissionDenied,
            2 | 3 | 55 | 1060 | 1168 => SdkError::DriverUnavailable,
            _ => SdkError::Win32 {
                op: op.to_string(),
                code,
                hint: Some("Is com0combus.sys loaded?".into()),
            },
        }
    }

    fn raw_ioctl(handle: HANDLE, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, u32> {
        let mut bytes_returned: u32 = 0;
        // SAFETY: all pointers/lengths describe live buffers for the duration
        // of the synchronous call.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                code,
                if input.is_empty() {
                    null::<c_void>()
                } else {
                    input.as_ptr() as *const c_void
                },
                input.len() as u32,
                if output.is_empty() {
                    null_mut::<c_void>()
                } else {
                    output.as_mut_ptr() as *mut c_void
                },
                output.len() as u32,
                &mut bytes_returned,
                null_mut::<OVERLAPPED>(),
            )
        };
        if ok == 0 {
            Err(unsafe { GetLastError() })
        } else {
            Ok(bytes_returned as usize)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use com0com_common::constants::GUID_COM0COM_BUS_BYTES;

        #[test]
        fn guid_from_bytes_matches_bus() {
            let g = guid_from_bytes(&GUID_COM0COM_BUS_BYTES);
            // GUID_COM0COM_BUS = 900C0CC0-2211-4433-5566-778899AABBCC
            assert_eq!(g.data1, 0x900C_0CC0);
            assert_eq!(g.data2, 0x2211);
            assert_eq!(g.data3, 0x4433);
            assert_eq!(g.data4, [0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC]);
        }
    }
}
