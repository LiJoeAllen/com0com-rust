//! COM port naming: create the `\DosDevices\COMx` symbolic link (so
//! `CreateFile("COMx")` works) and register the `GUID_DEVINTERFACE_COMPORT`
//! device interface for Device Manager / Comm API discovery.

use core::ptr;

use wdk_sys::{
    nt_success, IoCreateSymbolicLink, IoDeleteSymbolicLink, IoRegisterDeviceInterface,
    IoSetDeviceInterfaceState, NTSTATUS, PDEVICE_OBJECT, UNICODE_STRING,
};

use com0com_common::{
    constants::GUID_DEVINTERFACE_COMPORT_BYTES,
    guid::guid_parts,
    naming::{format_decimal, wide_to_ascii},
    pair::PairContext,
    protocol::Com0comPairParams,
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

/// Deprecated local helpers removed: `wide_to_ascii` / `format_decimal` now live
/// in `com0com_common::naming` (host-tested, shared with the bus driver).

pub unsafe fn register(device: PDEVICE_OBJECT, ext: *mut PortExtension) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        return wdk_sys::STATUS_SUCCESS;
    }
    let idx = (*ext).port_index as usize;
    let params: &Com0comPairParams = &(*pc).port[idx].params; // borrow; copy name out
    let raw_name = wide_to_ascii(&params.port_name[idx]);
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
    let dev_name = device_name_wide((*ext).pair_id, (*ext).port_index, &mut dev_buf);
    let mut dos_buf = [0u16; 48];
    let dos_name = dos_symlink_wide(&name, &mut dos_buf);

    let status = IoCreateSymbolicLink(
        &dos_name as *const UNICODE_STRING,
        &dev_name as *const UNICODE_STRING,
    );
    if nt_success(status) {
        (*ext).com_symlink = dos_name;
    }

    // Register + enable the COMPORT interface for enumeration.
    let mut iface: wdk_sys::PUNICODE_STRING = ptr::null_mut();
    let mut guid = guid_from_comport();
    let status2 = IoRegisterDeviceInterface(
        device,
        &guid as *const wdk_sys::GUID,
        ptr::null_mut(),
        &mut iface,
    );
    if nt_success(status2) && !iface.is_null() {
        let _ = IoSetDeviceInterfaceState(*iface, 1);
        wdk_sys::ExFreePool(*iface as wdk_sys::PVOID);
    }
    wdk_sys::STATUS_SUCCESS
}

/// Tear down the COMx symbolic link on device removal.
pub unsafe fn unregister(ext: *mut PortExtension) {
    if (*ext).com_symlink.Buffer.is_null() {
        return;
    }
    let _ = IoDeleteSymbolicLink(&(*ext).com_symlink as *const UNICODE_STRING);
}

fn guid_from_comport() -> wdk_sys::GUID {
    let p = guid_parts(&GUID_DEVINTERFACE_COMPORT_BYTES);
    wdk_sys::GUID {
        Data1: p.data1,
        Data2: p.data2,
        Data3: p.data3,
        Data4: p.data4,
    }
}
