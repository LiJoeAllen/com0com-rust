//! WDM DriverEntry, AddDevice, and the kernel-to-bus PORT_LINK handshake.

use core::ptr;

use wdk_sys::{
    nt_success, DevicePropertyCompatibleIDs, Executive, IoAttachDeviceToDeviceStack,
    IoBuildDeviceIoControlRequest, IoCallDriver, IoCreateDevice, IoDeleteDevice, IoDetachDevice,
    IoFreeIrp, IoGetDeviceInterfaces, IoGetDeviceObjectPointer, IoGetDeviceProperty, IoStatusBlock,
    KeInitializeEvent, KeWaitForSingleObject, DEVICE_INITIALIZING, DEVICE_OBJECT, DO_BUFFERED_IO,
    DRIVER_OBJECT, FILE_DEVICE_SERIAL_PORT, IRP_MJ_CLEANUP, IRP_MJ_CLOSE, IRP_MJ_CREATE,
    IRP_MJ_DEVICE_CONTROL, IRP_MJ_PNP, IRP_MJ_POWER, IRP_MJ_READ, IRP_MJ_SYSTEM_CONTROL,
    IRP_MJ_WRITE, KPROCESSOR_MODE, NTSTATUS, PDEVICE_OBJECT, PDRIVER_OBJECT, PIRP, PUNICODE_STRING,
    UNICODE_STRING,
};

use com0com_common::ioctls::IOCTL_COM0COM_PORT_LINK;
use com0com_common::protocol::{PortLinkRequest, PortLinkResponse};

use crate::{comname, dispatch};

#[repr(C)]
pub struct PortExtension {
    pub fdo: PDEVICE_OBJECT,
    pub lower_device: PDEVICE_OBJECT,
    pub pdo: PDEVICE_OBJECT,
    pub pair_id: u32,
    pub port_index: u8,
    pub pair_va: u64,
    pub com_symlink: UNICODE_STRING, // valid Length != 0 once registered
}

#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver: PDRIVER_OBJECT,
    _registry_path: *mut UNICODE_STRING,
) -> NTSTATUS {
    (*driver).DriverUnload = Some(driver_unload);
    let ext = (*driver).DriverExtension;
    if !ext.is_null() {
        (*ext).AddDevice = Some(add_device);
    }
    let major = &mut (*driver).MajorFunction;
    major[IRP_MJ_CREATE as usize] = Some(dispatch::dispatch_create_close);
    major[IRP_MJ_CLOSE as usize] = Some(dispatch::dispatch_create_close);
    major[IRP_MJ_CLEANUP as usize] = Some(dispatch::dispatch_cleanup);
    major[IRP_MJ_READ as usize] = Some(dispatch::dispatch_read);
    major[IRP_MJ_WRITE as usize] = Some(dispatch::dispatch_write);
    major[IRP_MJ_DEVICE_CONTROL as usize] = Some(dispatch::dispatch_device_control);
    major[IRP_MJ_PNP as usize] = Some(dispatch::dispatch_pnp);
    major[IRP_MJ_POWER as usize] = Some(dispatch::dispatch_power);
    major[IRP_MJ_SYSTEM_CONTROL as usize] = Some(dispatch::dispatch_system_control);
    wdk_sys::STATUS_SUCCESS
}

unsafe extern "C" fn driver_unload(_driver: PDRIVER_OBJECT) {
    // No global teardown needed; each device cleans up on REMOVE_DEVICE.
}

unsafe extern "C" fn add_device(driver: PDRIVER_OBJECT, pdo: PDEVICE_OBJECT) -> NTSTATUS {
    let (pair_id, port_index) = match parse_pair_from_pdo(pdo) {
        Some(v) => v,
        None => return wdk_sys::STATUS_INVALID_PARAMETER,
    };

    let mut device: PDEVICE_OBJECT = ptr::null_mut();
    let size = core::mem::size_of::<PortExtension>() as u32;
    let mut dev_name_buf = [0u16; 48];
    let dev_name = comname::device_name_wide(pair_id, port_index, &mut dev_name_buf);
    let status = IoCreateDevice(
        driver,
        size,
        &dev_name as *const UNICODE_STRING,
        FILE_DEVICE_SERIAL_PORT,
        0,
        0,
        &mut device,
    );
    if !nt_success(status) {
        return status;
    }

    let ext = (*device).DeviceExtension as *mut PortExtension;
    ptr::write_bytes(ext as *mut u8, 0u8, core::mem::size_of::<PortExtension>());
    (*ext).fdo = device;
    (*ext).pdo = pdo;
    (*ext).pair_id = pair_id;
    (*ext).port_index = port_index;

    let lower = IoAttachDeviceToDeviceStack(device, pdo);
    if lower.is_null() {
        IoDeleteDevice(device);
        return wdk_sys::STATUS_NO_SUCH_DEVICE;
    }
    (*ext).lower_device = lower;

    // Link to the bus to obtain the shared PairContext.
    let pair_va = link_to_bus(pair_id, port_index);
    if pair_va == 0 {
        IoDetachDevice(lower);
        IoDeleteDevice(device);
        return wdk_sys::STATUS_UNSUCCESSFUL;
    }
    (*ext).pair_va = pair_va;

    comname::register(device, ext);

    (*device).Flags = ((*device).Flags | DO_BUFFERED_IO) & !DEVICE_INITIALIZING;
    wdk_sys::STATUS_SUCCESS
}

/// Read the PDO compatible IDs (`c0c0_<hex8>_<A|B>`) to recover the pair id.
unsafe fn parse_pair_from_pdo(pdo: PDEVICE_OBJECT) -> Option<(u32, u8)> {
    let mut len: u32 = 0;
    let status = IoGetDeviceProperty(
        pdo,
        DevicePropertyCompatibleIDs,
        0,
        ptr::null_mut(),
        &mut len,
        ptr::null_mut(),
    );
    if status != wdk_sys::STATUS_BUFFER_TOO_SMALL && status != wdk_sys::STATUS_BUFFER_OVERFLOW {
        return None;
    }
    let buf = wdk_sys::ExAllocatePool2(
        wdk_sys::POOL_FLAG_NON_PAGED,
        len as u64,
        u32::from_le_bytes(*b"cprt"),
    );
    if buf.is_null() {
        return None;
    }
    let status = IoGetDeviceProperty(
        pdo,
        DevicePropertyCompatibleIDs,
        len,
        buf,
        &mut len,
        ptr::null_mut(),
    );
    if !nt_success(status) {
        wdk_sys::ExFreePool(buf);
        return None;
    }
    let result = parse_compat_ids(buf as *const u16, len as usize);
    wdk_sys::ExFreePool(buf);
    result
}

unsafe fn parse_compat_ids(wide: *const u16, len_bytes: usize) -> Option<(u32, u8)> {
    let n = len_bytes / 2; // u16 units
    let mut i = 0;
    while i < n {
        // Find a string beginning with "c0c0_".
        if wide.add(i).read() == b'c' as u16
            && wide.add(i + 1).read() == b'0' as u16
            && wide.add(i + 2).read() == b'c' as u16
            && wide.add(i + 3).read() == b'0' as u16
            && wide.add(i + 4).read() == b'_' as u16
        {
            let mut pair_id: u32 = 0;
            let mut ok = true;
            for k in 0..8 {
                let c = wide.add(i + 5 + k).read() as u8 as char;
                let v = match c {
                    '0'..='9' => c as u32 - '0' as u32,
                    'a'..='f' => c as u32 - 'a' as u32 + 10,
                    _ => {
                        ok = false;
                        break;
                    }
                };
                pair_id = (pair_id << 4) | v;
            }
            if !ok {
                return None;
            }
            // role char after the 8 hex digits + '_'
            let role = wide.add(i + 5 + 8 + 1).read() as u8;
            let idx = if role == b'A' { 0u8 } else { 1u8 };
            return Some((pair_id, idx));
        }
        // skip to next null-terminated string
        while i < n && wide.add(i).read() != 0 {
            i += 1;
        }
        i += 1; // skip the null
    }
    None
}

/// Open the bus control device interface and issue PORT_LINK to fetch the
/// shared `PairContext` VA.
unsafe fn link_to_bus(pair_id: u32, port_index: u8) -> u64 {
    let mut iface: PUNICODE_STRING = ptr::null_mut();
    let mut guid = crate::comname::bus_guid();
    let status = IoGetDeviceInterfaces(
        &guid as *const wdk_sys::GUID,
        ptr::null_mut(),
        0,
        &mut iface,
    );
    if !nt_success(status) || iface.is_null() || (*iface).Buffer.is_null() {
        return 0;
    }
    let mut file_obj: *mut wdk_sys::FILE_OBJECT = ptr::null_mut();
    let mut dev_obj: PDEVICE_OBJECT = ptr::null_mut();
    let status =
        IoGetDeviceObjectPointer(iface, wdk_sys::FILE_READ_DATA, &mut file_obj, &mut dev_obj);
    wdk_sys::ExFreePool(iface as wdk_sys::PVOID);
    if !nt_success(status) || dev_obj.is_null() {
        return 0;
    }

    let req = PortLinkRequest {
        pair_id,
        port_index,
    };
    let mut in_buf = req;
    let mut out_buf = PortLinkResponse { pair_va: 0 };

    let mut event = core::mem::zeroed::<wdk_sys::KEVENT>();
    KeInitializeEvent(&mut event, 1 /* NotificationEvent */, 0);
    let mut iosb = core::mem::zeroed::<IoStatusBlock>();

    let irp = IoBuildDeviceIoControlRequest(
        IOCTL_COM0COM_PORT_LINK,
        dev_obj,
        &mut in_buf as *mut _ as *mut core::ffi::c_void,
        core::mem::size_of::<PortLinkRequest>() as u32,
        &mut out_buf as *mut _ as *mut core::ffi::c_void,
        core::mem::size_of::<PortLinkResponse>() as u32,
        0, // InternalDeviceControl = FALSE (IRP_MJ_DEVICE_CONTROL)
        &mut event as *mut wdk_sys::KEVENT,
        &mut iosb as *mut IoStatusBlock,
    );
    if irp.is_null() {
        return 0;
    }
    let status = IoCallDriver(dev_obj, irp);
    if status == wdk_sys::STATUS_PENDING {
        KeWaitForSingleObject(
            &mut event as *mut wdk_sys::KEVENT as *mut core::ffi::c_void,
            0, // Executive
            0, // KernelMode
            0, // not alertable
            ptr::null_mut(),
        );
    }
    if nt_success(iosb.Status) {
        out_buf.pair_va
    } else {
        0
    }
}
