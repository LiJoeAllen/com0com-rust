//! COM port naming: create the `\DosDevices\COMx` symbolic link (so
//! `CreateFile("COMx")` works) and register the `GUID_DEVINTERFACE_COMPORT`
//! device interface for Device Manager / Comm API discovery.

use core::ptr;

use wdk_sys::NT_SUCCESS as nt_success;
use wdk_sys::{
    ntddk::{
        ExFreePool, IoCreateSymbolicLink, IoDeleteSymbolicLink, IoRegisterDeviceInterface,
        IoSetDeviceInterfaceState, KeAcquireSpinLockRaiseToDpc, KeReleaseSpinLock,
    },
    KSPIN_LOCK, NTSTATUS, UNICODE_STRING,
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
        // No explicit name: derive a deterministic default. Real com0com uses
        // the COM Name Arbiter; for the MVP we pick COMN where N = pair_id + 2.
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
