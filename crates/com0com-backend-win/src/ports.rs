//! Serial port enumeration for [`crate::WinBackend::list_ports`].
//!
//! The list is built from the `GUID_DEVINTERFACE_COMPORT` device interfaces so
//! it also contains ports that com0com does not manage. Managed ports created
//! by `com0combus` carry an instance ID of the form `c0c0_<hex8><A|B>`, which
//! is parsed back into `(pair_id, port_index)`.

#[cfg(any(windows, test))]
use com0com_core::{PairId, PortIndex};
use com0com_core::{PortInfo, SdkError};

/// Parsed `c0c0_<hex8><A|B>` instance ID.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedPort {
    pub pair_id: PairId,
    pub port_index: PortIndex,
}

/// Parse a PnP instance ID produced by `com0combus`.
///
/// Windows upper-cases instance IDs in some code paths, so the prefix and the
/// role character are matched case-insensitively. Values above `MAX_PAIR_ID`
/// are rejected: the driver cannot have created them, so they are not ours.
#[cfg(any(windows, test))]
pub(crate) fn parse_managed_instance_id(instance: &str) -> Option<ManagedPort> {
    // SetupAPI returns the full device instance ID, e.g.
    // `COM0COM\PORT_A\1&79f5d87&1&c0c0_00000003A`; the child id the bus driver
    // assigned is the last 14 characters.
    let instance = instance.get(instance.len().checked_sub(14)?..)?;
    if !instance.is_ascii() {
        return None;
    }
    let bytes = instance.as_bytes();
    if !bytes[..5].eq_ignore_ascii_case(b"c0c0_") {
        return None;
    }
    let pair_id = u32::from_str_radix(instance.get(5..13)?, 16).ok()?;
    let port_index = match bytes[13] {
        b'A' | b'a' => 0,
        b'B' | b'b' => 1,
        _ => return None,
    };
    if pair_id > com0com_core::MAX_PAIR_ID as u32 {
        return None;
    }
    Some(ManagedPort {
        pair_id: pair_id as PairId,
        port_index,
    })
}

pub(crate) fn list_ports() -> Result<Vec<PortInfo>, SdkError> {
    #[cfg(windows)]
    {
        windows::list()
    }
    #[cfg(not(windows))]
    {
        Err(SdkError::Unsupported)
    }
}

#[cfg(windows)]
mod windows {
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::errhandlingapi::GetLastError;
    use windows_sys::Win32::handleapi::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::setupapi::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW, SetupDiOpenDevRegKey,
        DICS_FLAG_GLOBAL, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, DIREG_DEV, HDEVINFO,
        SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
    };
    use windows_sys::Win32::winnt::KEY_READ;
    use windows_sys::Win32::winreg::{RegCloseKey, RegQueryValueExW};

    use com0com_common::constants::GUID_DEVINTERFACE_COMPORT;
    use com0com_core::{PortInfo, SdkError};

    use super::parse_managed_instance_id;
    use crate::transport::guid_from_bytes;

    struct DeviceInfoList(HDEVINFO);

    impl Drop for DeviceInfoList {
        fn drop(&mut self) {
            // SAFETY: the handle came from `SetupDiGetClassDevsW` and is owned
            // by this guard.
            unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    pub(super) fn list() -> Result<Vec<PortInfo>, SdkError> {
        let guid = guid_from_bytes(&GUID_DEVINTERFACE_COMPORT);
        // SAFETY: plain SetupAPI call with a null parent window.
        let hdev = unsafe {
            SetupDiGetClassDevsW(
                &guid,
                null(),
                null_mut(),
                (DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) as u32,
            )
        };
        if hdev == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            return match code {
                // No ports at all is a valid, empty list.
                2 | 1168 | 259 => Ok(Vec::new()),
                5 => Err(SdkError::PermissionDenied),
                _ => Err(SdkError::Win32 {
                    op: "SetupDiGetClassDevsW".into(),
                    code,
                    hint: None,
                }),
            };
        }
        let hdev = DeviceInfoList(hdev);

        let mut ports = Vec::new();
        let mut index = 0u32;
        loop {
            let mut interface: SP_DEVICE_INTERFACE_DATA = unsafe { zeroed() };
            interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            // SAFETY: `hdev.0` is a live device info set and `interface` is
            // fully initialised.
            let ok = unsafe {
                SetupDiEnumDeviceInterfaces(hdev.0, null(), &guid, index, &mut interface)
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == 259 {
                    // ERROR_NO_MORE_ITEMS: enumeration finished.
                    break;
                }
                return Err(SdkError::Win32 {
                    op: "SetupDiEnumDeviceInterfaces".into(),
                    code,
                    hint: None,
                });
            }
            index += 1;

            let Some(device) = interface_device(hdev.0, &interface) else {
                continue;
            };
            let Some(instance) = instance_id(hdev.0, &device) else {
                continue;
            };
            let Some(name) = port_name(hdev.0, &device) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let managed = parse_managed_instance_id(&instance);
            ports.push(PortInfo {
                name,
                pair_id: managed.map(|m| m.pair_id),
                port_index: managed.map(|m| m.port_index),
                managed: managed.is_some(),
            });
        }
        Ok(ports)
    }

    /// Resolve the `SP_DEVINFO_DATA` behind one interface.
    fn interface_device(
        hdev: HDEVINFO,
        interface: &SP_DEVICE_INTERFACE_DATA,
    ) -> Option<SP_DEVINFO_DATA> {
        let mut required: u32 = 0;
        // SAFETY: size probe with a null detail buffer is explicitly allowed.
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                hdev,
                interface,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            );
        }
        if required == 0 {
            return None;
        }
        // The detail struct is a fixed header followed by a wide path; allocate
        // in `usize` units so the alignment requirement is satisfied.
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut buf: Vec<usize> = vec![0; words];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        let detail_cb_size: u32 = if size_of::<usize>() == 8 { 8 } else { 6 };
        // SAFETY: the allocation covers `required` bytes.
        unsafe {
            (*detail).cbSize = detail_cb_size;
        }
        let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
        device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        // SAFETY: buffers are sized from the probe result.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                hdev,
                interface,
                detail,
                required,
                null_mut(),
                &mut device,
            )
        };
        (ok != 0).then_some(device)
    }

    fn instance_id(hdev: HDEVINFO, device: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buf = [0u16; 256];
        let mut required: u32 = 0;
        // SAFETY: the buffer length is passed in `u16` units.
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(
                hdev,
                device,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut required,
            )
        };
        if ok == 0 {
            return None;
        }
        let len = (required as usize).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// Read the `PortName` registry value (`COM15`) of a port device.
    fn port_name(hdev: HDEVINFO, device: &SP_DEVINFO_DATA) -> Option<String> {
        // SAFETY: `device` is a live SP_DEVINFO_DATA from this device set.
        let key = unsafe {
            SetupDiOpenDevRegKey(
                hdev,
                device,
                DICS_FLAG_GLOBAL as u32,
                0,
                DIREG_DEV as u32,
                KEY_READ as u32,
            )
        };
        if key == INVALID_HANDLE_VALUE {
            return None;
        }
        let value: Vec<u16> = "PortName\0".encode_utf16().collect();
        let mut kind: u32 = 0;
        let mut buf = [0u16; 256];
        let mut bytes = (buf.len() * size_of::<u16>()) as u32;
        // SAFETY: `value` is NUL-terminated; `buf`/`bytes` describe a live
        // output buffer.
        let status = unsafe {
            RegQueryValueExW(
                key,
                value.as_ptr(),
                null(),
                &mut kind,
                buf.as_mut_ptr() as *mut u8,
                &mut bytes,
            )
        };
        // SAFETY: `key` is owned by this function.
        unsafe { RegCloseKey(key) };
        if status != 0 {
            return None;
        }
        let units = (bytes as usize / size_of::<u16>()).min(buf.len());
        let len = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_driver_instance_ids() {
        let a = parse_managed_instance_id("c0c0_00000000A").unwrap();
        assert_eq!((a.pair_id, a.port_index), (0, 0));
        let b = parse_managed_instance_id("C0C0_0000003FB").unwrap();
        assert_eq!((b.pair_id, b.port_index), (63, 1));
        let mixed = parse_managed_instance_id("C0c0_0000002aB").unwrap();
        assert_eq!((mixed.pair_id, mixed.port_index), (42, 1));
        let full = parse_managed_instance_id(r"COM0COM\PORT_A\1&79F5D87&1&C0C0_00000000A").unwrap();
        assert_eq!((full.pair_id, full.port_index), (0, 0));
        let full_b =
            parse_managed_instance_id(r"COM0COM\PORT_B\1&79F5D87&1&C0C0_0000003FB").unwrap();
        assert_eq!((full_b.pair_id, full_b.port_index), (63, 1));
    }

    #[test]
    fn rejects_foreign_and_malformed_instance_ids() {
        assert_eq!(parse_managed_instance_id("ROOT\\PORTS\\0000"), None);
        assert_eq!(parse_managed_instance_id("c0c0_00000040A"), None);
        assert_eq!(parse_managed_instance_id("c0c0_0000000ZA"), None);
        assert_eq!(parse_managed_instance_id("c0c0_0000000AC"), None);
        assert_eq!(parse_managed_instance_id("c0c0_0000000A"), None);
        assert_eq!(parse_managed_instance_id("c0c0_"), None);
        assert_eq!(parse_managed_instance_id(""), None);
    }
}
