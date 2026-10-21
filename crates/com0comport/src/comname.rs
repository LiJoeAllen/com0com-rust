//! COM port naming: create the `\DosDevices\COMx` symbolic link (so
//! `CreateFile("COMx")` works) and register the `GUID_DEVINTERFACE_COMPORT`
//! device interface for Device Manager / Comm API discovery.

use core::ptr;

use wdk_sys::NT_SUCCESS as nt_success;
use wdk_sys::{
    ntddk::{
        ExFreePool, IoCreateSymbolicLink, IoDeleteSymbolicLink, IoOpenDeviceRegistryKey,
        IoRegisterDeviceInterface, IoSetDeviceInterfaceState, KeAcquireSpinLockRaiseToDpc,
        KeReleaseSpinLock, RtlCreateRegistryKey, RtlDeleteRegistryValue, RtlWriteRegistryValue,
        ZwClose, ZwQueryValueKey, ZwSetValueKey,
    },
    HANDLE, KEY_READ, KEY_SET_VALUE, KSPIN_LOCK, NTSTATUS, PLUGPLAY_REGKEY_DEVICE, REG_SZ,
    RTL_REGISTRY_ABSOLUTE, UNICODE_STRING,
};

use com0com_common::{
    constants::GUID_DEVINTERFACE_COMPORT,
    guid::guid_parts,
    naming::{format_decimal, wide_to_ascii},
    pair::PairContext,
    protocol::PAIR_PARAMS_NAME_LEN,
};

use crate::driver::PortExtension;

/// `GUID_COM0COM_BUS` for the kernel-side `link_to_bus` handshake.
pub fn bus_guid() -> wdk_sys::GUID {
    let p = guid_parts(&com0com_common::constants::GUID_COM0COM_BUS_BYTES);
    wdk_sys::GUID {
        Data1: p.data1,
        Data2: p.data2,
        Data3: p.data3,
        Data4: p.data4,
    }
}

/// Build the device name `\Device\com0comport_<hex8>_<A|B>` as a UNICODE_STRING.
///
/// # Safety
/// `buf` must outlive the returned `UNICODE_STRING`, which aliases it.
pub unsafe fn device_name_wide(
    pair_id: u32,
    port_index: u8,
    buf: &mut [u16; 48],
) -> UNICODE_STRING {
    let prefix = b"\\Device\\com0comport_";
    let mut i = 0;
    while i < prefix.len() && i < buf.len() {
        buf[i] = prefix[i] as u16;
        i += 1;
    }
    for k in (0..8).rev() {
        let nib = ((pair_id >> (k * 4)) & 0xf) as u8;
        if i < buf.len() {
            buf[i] = if nib < 10 {
                b'0' + nib
            } else {
                b'a' + (nib - 10)
            } as u16;
            i += 1;
        }
    }
    if i < buf.len() {
        buf[i] = (if port_index == 0 { b'A' } else { b'B' }) as u16;
        i += 1;
    }
    buf[i] = 0;
    UNICODE_STRING {
        Length: (i * 2) as u16,
        MaximumLength: (buf.len() * 2) as u16,
        Buffer: buf.as_mut_ptr(),
    }
}

/// Build `\DosDevices\<name>` from an ASCII COM name.
unsafe fn dos_symlink_wide(name: &[u8], buf: &mut [u16; 48]) -> UNICODE_STRING {
    let prefix = b"\\DosDevices\\";
    let mut i = 0;
    while i < prefix.len() && i < buf.len() {
        buf[i] = prefix[i] as u16;
        i += 1;
    }
    for &c in name {
        if c == 0 || i >= buf.len() - 1 {
            break;
        }
        buf[i] = c as u16;
        i += 1;
    }
    buf[i] = 0;
    UNICODE_STRING {
        Length: (i * 2) as u16,
        MaximumLength: (buf.len() * 2) as u16,
        Buffer: buf.as_mut_ptr(),
    }
}

/// Copy NUL-terminated ASCII into `buf` as UTF-16 (with trailing NUL).
/// Returns the number of characters written, excluding the NUL.
unsafe fn ascii_to_wide(ascii: &[u8], buf: &mut [u16]) -> usize {
    let mut i = 0;
    while i < ascii.len() && i < buf.len() - 1 && ascii[i] != 0 {
        buf[i] = ascii[i] as u16;
        i += 1;
    }
    buf[i] = 0;
    i
}

/// `\Registry\Machine\HARDWARE\DEVICEMAP\SERIALCOMM`, NUL-terminated.
unsafe fn serialcomm_path(buf: &mut [u16; 64]) -> usize {
    ascii_to_wide(b"\\Registry\\Machine\\HARDWARE\\DEVICEMAP\\SERIALCOMM", buf)
}

/// Publish the real COM name so Device Manager and `GetPortNames()` see it.
///
/// The Ports class co-installer assigns its own number at install time
/// (`Device Parameters\PortName`), which does not match the name configured in
/// the pair. The driver owns the authoritative name, so it overwrites both the
/// device key value and the `SERIALCOMM` devicemap entry.
unsafe fn publish_port_name(ext: *mut PortExtension, name: &[u8]) {
    let mut handle: HANDLE = ptr::null_mut();
    let status = IoOpenDeviceRegistryKey(
        (*ext).pdo,
        PLUGPLAY_REGKEY_DEVICE,
        KEY_SET_VALUE,
        &mut handle,
    );
    if nt_success(status) && !handle.is_null() {
        let mut vn_buf = [0u16; 16];
        let vn_len = ascii_to_wide(b"PortName", &mut vn_buf);
        let mut value_name = UNICODE_STRING {
            Length: (vn_len * 2) as u16,
            MaximumLength: ((vn_len + 1) * 2) as u16,
            Buffer: vn_buf.as_mut_ptr(),
        };
        let mut data = [0u16; 32];
        let n = ascii_to_wide(name, &mut data);
        let _ = ZwSetValueKey(
            handle,
            &mut value_name,
            0,
            REG_SZ,
            data.as_mut_ptr() as *mut core::ffi::c_void,
            ((n + 1) * 2) as u32,
        );
        let _ = ZwClose(handle);
    }

    let mut path_buf = [0u16; 64];
    serialcomm_path(&mut path_buf);
    let _ = RtlCreateRegistryKey(RTL_REGISTRY_ABSOLUTE, path_buf.as_ptr());
    let mut dev_buf = [0u16; 48];
    let dev_name = device_name_wide((*ext).pair_id, (*ext).port_index, &mut dev_buf);
    let mut data = [0u16; 32];
    let n = ascii_to_wide(name, &mut data);
    let _ = RtlWriteRegistryValue(
        RTL_REGISTRY_ABSOLUTE,
        path_buf.as_ptr(),
        dev_name.Buffer,
        REG_SZ,
        data.as_mut_ptr() as *mut core::ffi::c_void,
        ((n + 1) * 2) as u32,
    );
}

/// Remove this port's `SERIALCOMM` entry when the device goes away.
unsafe fn unpublish_port_name(ext: *mut PortExtension) {
    let mut path_buf = [0u16; 64];
    serialcomm_path(&mut path_buf);
    let mut dev_buf = [0u16; 48];
    let dev_name = device_name_wide((*ext).pair_id, (*ext).port_index, &mut dev_buf);
    let _ = RtlDeleteRegistryValue(RTL_REGISTRY_ABSOLUTE, path_buf.as_ptr(), dev_name.Buffer);
}

/// Read the `Device Parameters\PortName` value assigned by the Ports class
/// co-installer, which asks the COM Name Arbiter for a unique name.
///
/// Returns `false` unless the value is a `REG_SZ` beginning with `COM`.
unsafe fn read_installed_port_name(ext: *mut PortExtension, name: &mut [u8; 32]) -> bool {
    let mut handle: HANDLE = ptr::null_mut();
    let status = IoOpenDeviceRegistryKey((*ext).pdo, PLUGPLAY_REGKEY_DEVICE, KEY_READ, &mut handle);
    if !nt_success(status) || handle.is_null() {
        return false;
    }
    let mut vn_buf = [0u16; 16];
    let vn_len = ascii_to_wide(b"PortName", &mut vn_buf);
    let mut value_name = UNICODE_STRING {
        Length: (vn_len * 2) as u16,
        MaximumLength: ((vn_len + 1) * 2) as u16,
        Buffer: vn_buf.as_mut_ptr(),
    };
    // 128 字节且 4 字节对齐：KEY_VALUE_PARTIAL_INFORMATION 头部（12 字节）
    // 后面直接跟 REG_SZ 数据。
    let mut buf = [0u32; 32];
    let mut result_len: u32 = 0;
    let status = ZwQueryValueKey(
        handle,
        &mut value_name,
        wdk_sys::_KEY_VALUE_INFORMATION_CLASS::KeyValuePartialInformation,
        buf.as_mut_ptr() as *mut core::ffi::c_void,
        core::mem::size_of_val(&buf) as u32,
        &mut result_len,
    );
    let _ = ZwClose(handle);
    if !nt_success(status) {
        return false;
    }
    let info = &*(buf.as_ptr() as *const wdk_sys::KEY_VALUE_PARTIAL_INFORMATION);
    if info.Type != wdk_sys::REG_SZ || info.DataLength < 8 {
        return false;
    }
    let units = ((info.DataLength as usize) / 2).min(31);
    let data = (buf.as_ptr() as *const u8).add(core::mem::offset_of!(
        wdk_sys::KEY_VALUE_PARTIAL_INFORMATION,
        Data
    )) as *const u16;
    let mut wide = [0u16; 32];
    for i in 0..units {
        wide[i] = data.add(i).read();
    }
    let converted = wide_to_ascii(&wide);
    if converted[0] != b'C' || converted[1] != b'O' || converted[2] != b'M' {
        return false;
    }
    *name = converted;
    true
}

/// Register the DOS symlink and the COMPORT device interface.
///
/// # Safety
/// `ext` must point to a live `PortExtension` whose `pair_va`, `pdo` and
/// `com_symlink_buffer` remain valid for the lifetime of the device.
pub unsafe fn register(ext: *mut PortExtension) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        return wdk_sys::STATUS_SUCCESS;
    }
    let idx = (*ext).port_index as usize;
    // `set_params` overwrites `params` under the pair spinlock from an arbitrary
    // thread; copy the name in 32 u16 units under the same lock so this read is
    // never torn.
    let lock = &mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK;
    let irql = KeAcquireSpinLockRaiseToDpc(lock);
    let mut name_buf = [0u16; PAIR_PARAMS_NAME_LEN];
    name_buf.copy_from_slice(&(*pc).params.port_name[idx]);
    KeReleaseSpinLock(lock, irql);

    let raw_name = wide_to_ascii(&name_buf);
    let name = if raw_name[0] == 0 {
        // No explicit name: use the one the COM Name Arbiter already assigned
        // to this devnode (the Ports co-installer stores it as PortName).
        // Deriving COM(pair_id + 2) instead hands both ports the *same* name,
        // so Port B's symlink creation fails and the port never reaches
        // SERIALCOMM / GetPortNames().
        let mut installed = [0u8; 32];
        if read_installed_port_name(ext, &mut installed) {
            installed
        } else {
            let n = (*ext).pair_id + 2;
            let mut tmp = [0u8; 32];
            let mut j = 0;
            for c in b"COM".iter() {
                tmp[j] = *c;
                j += 1;
            }
            let digits = format_decimal(n);
            for d in digits.iter() {
                if j < tmp.len() - 1 {
                    tmp[j] = *d;
                    j += 1;
                }
            }
            tmp
        }
    } else {
        raw_name
    };

    let mut dev_buf = [0u16; 48];
    // device name already assigned at IoCreateDevice; recompute to link to it.
    let mut dev_name = device_name_wide((*ext).pair_id, (*ext).port_index, &mut dev_buf);
    let mut dos_name = dos_symlink_wide(&name, &mut (*ext).com_symlink_buffer);

    let status = IoCreateSymbolicLink(&mut dos_name, &mut dev_name);
    if nt_success(status) {
        (*ext).com_symlink = dos_name;
        publish_port_name(ext, &name);
    }

    // Register + enable the COMPORT interface for enumeration.
    let mut iface = UNICODE_STRING::default();
    let guid = guid_from_comport();
    let status2 = IoRegisterDeviceInterface(
        (*ext).pdo,
        &guid as *const wdk_sys::GUID,
        ptr::null_mut(),
        &mut iface,
    );
    if nt_success(status2) && !iface.Buffer.is_null() {
        let _ = IoSetDeviceInterfaceState(&mut iface, 1);
        ExFreePool(iface.Buffer as wdk_sys::PVOID);
    }
    wdk_sys::STATUS_SUCCESS
}

/// Tear down the COMx symbolic link on device removal.
///
/// # Safety
/// `ext` must be the live extension of the device being removed.
pub unsafe fn unregister(ext: *mut PortExtension) {
    if (*ext).com_symlink.Buffer.is_null() {
        return;
    }
    let _ = IoDeleteSymbolicLink(&mut (*ext).com_symlink);
    unpublish_port_name(ext);
}

fn guid_from_comport() -> wdk_sys::GUID {
    let p = guid_parts(&GUID_DEVINTERFACE_COMPORT);
    wdk_sys::GUID {
        Data1: p.data1,
        Data2: p.data2,
        Data3: p.data3,
        Data4: p.data4,
    }
}
