//! WDM DriverEntry, AddDevice, and the kernel-to-bus PORT_LINK handshake.

use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use wdk_sys::NT_SUCCESS as nt_success;
use wdk_sys::{
    ntddk::{
        ExAllocatePool2, ExFreePool, IoAttachDeviceToDeviceStack, IoBuildDeviceIoControlRequest,
        IoCreateDevice, IoDeleteDevice, IoDetachDevice, IoGetDeviceInterfaces,
        IoGetDeviceObjectPointer, IoGetDeviceProperty, IofCallDriver, KeInitializeEvent,
        KeWaitForSingleObject, ObfDereferenceObject,
    },
    DO_BUFFERED_IO, DO_DEVICE_INITIALIZING, FILE_DEVICE_SERIAL_PORT, IO_STATUS_BLOCK,
    IRP_MJ_CLEANUP, IRP_MJ_CLOSE, IRP_MJ_CREATE, IRP_MJ_DEVICE_CONTROL, IRP_MJ_PNP, IRP_MJ_POWER,
    IRP_MJ_READ, IRP_MJ_SYSTEM_CONTROL, IRP_MJ_WRITE, NTSTATUS, PDEVICE_OBJECT, PDRIVER_CANCEL,
    PDRIVER_OBJECT, PIO_COMPLETION_ROUTINE, PIO_STACK_LOCATION, PIRP, PZZWSTR, UNICODE_STRING,
};

use com0com_common::ioctls::{IOCTL_COM0COM_PORT_LINK, IOCTL_COM0COM_PORT_UNLINK};
use com0com_common::protocol::{PortLinkRequest, PortLinkResponse, PortUnlinkRequest};

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
    pub com_symlink_buffer: [u16; 48],
}

/// Return the current I/O stack location using the WDK inline layout.
#[inline]
pub(crate) unsafe fn current_irp_stack_location(irp: PIRP) -> PIO_STACK_LOCATION {
    (*irp)
        .Tail
        .Overlay
        .__bindgen_anon_2
        .__bindgen_anon_1
        .CurrentStackLocation
}

/// Advance an IRP past this driver without changing its stack parameters.
#[inline]
pub(crate) unsafe fn skip_current_irp_stack_location(irp: PIRP) {
    (*irp).CurrentLocation += 1;
    let current = &mut (*irp)
        .Tail
        .Overlay
        .__bindgen_anon_2
        .__bindgen_anon_1
        .CurrentStackLocation;
    *current = (*current).add(1);
}

/// Mark the IRP pending exactly as the WDK inline helper does.
#[inline]
pub(crate) unsafe fn mark_irp_pending(irp: PIRP) {
    (*current_irp_stack_location(irp)).Control |= wdk_sys::SL_PENDING_RETURNED as u8;
}

/// Atomically replace an IRP cancel routine and return its previous value.
///
/// This mirrors the WDK `IoSetCancelRoutine` inline helper. `PDRIVER_CANCEL`
/// is an `Option<unsafe extern "C" fn(...)>` in the generated bindings, so
/// `Option::is_some()` remains the ownership test used by the cancel protocol.
#[inline]
pub(crate) unsafe fn set_cancel_routine(irp: PIRP, routine: PDRIVER_CANCEL) -> PDRIVER_CANCEL {
    let new_routine = routine.map_or(ptr::null_mut(), |callback| callback as *mut c_void);
    let slot = &*core::ptr::addr_of_mut!((*irp).CancelRoutine).cast::<AtomicPtr<c_void>>();
    let previous = slot.swap(new_routine, Ordering::SeqCst);
    core::mem::transmute::<*mut c_void, PDRIVER_CANCEL>(previous)
}

/// Copy the current IRP parameters to the next stack location.
#[inline]
pub(crate) unsafe fn copy_current_irp_stack_location_to_next(irp: PIRP) {
    let current = current_irp_stack_location(irp);
    let next = current.sub(1);
    let parameter_bytes = core::mem::offset_of!(wdk_sys::IO_STACK_LOCATION, CompletionRoutine);
    ptr::copy_nonoverlapping(current.cast::<u8>(), next.cast::<u8>(), parameter_bytes);
    (*next).Control = 0;
}

/// Install a completion routine in the next IRP stack location.
#[inline]
pub(crate) unsafe fn set_completion_routine(
    irp: PIRP,
    routine: PIO_COMPLETION_ROUTINE,
    context: *mut c_void,
    invoke_on_success: bool,
    invoke_on_error: bool,
    invoke_on_cancel: bool,
) {
    let stack = current_irp_stack_location(irp).sub(1);
    (*stack).CompletionRoutine = routine;
    (*stack).Context = context;
    (*stack).Control = 0;
    if invoke_on_success {
        (*stack).Control |= wdk_sys::SL_INVOKE_ON_SUCCESS as u8;
    }
    if invoke_on_error {
        (*stack).Control |= wdk_sys::SL_INVOKE_ON_ERROR as u8;
    }
    if invoke_on_cancel {
        (*stack).Control |= wdk_sys::SL_INVOKE_ON_CANCEL as u8;
    }
}

/// # Safety
/// Invoked by the I/O manager with a valid `DRIVER_OBJECT`; never call directly.
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
    let mut dev_name = comname::device_name_wide(pair_id, port_index, &mut dev_name_buf);
    let status = IoCreateDevice(
        driver,
        size,
        &mut dev_name,
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

    comname::register(ext);

    (*device).Flags = ((*device).Flags | DO_BUFFERED_IO) & !DO_DEVICE_INITIALIZING;
    wdk_sys::STATUS_SUCCESS
}

/// Read the PDO compatible IDs (`c0c0_<hex8>_<A|B>`) to recover the pair id.
unsafe fn parse_pair_from_pdo(pdo: PDEVICE_OBJECT) -> Option<(u32, u8)> {
    let mut len: u32 = 0;
    let status = IoGetDeviceProperty(
        pdo,
        wdk_sys::DEVICE_REGISTRY_PROPERTY::DevicePropertyCompatibleIDs,
        0,
        ptr::null_mut(),
        &mut len,
    );
    if status != wdk_sys::STATUS_BUFFER_TOO_SMALL && status != wdk_sys::STATUS_BUFFER_OVERFLOW {
        return None;
    }
    let buf = ExAllocatePool2(
        wdk_sys::POOL_FLAG_NON_PAGED,
        len as u64,
        u32::from_le_bytes(*b"cprt"),
    );
    if buf.is_null() {
        return None;
    }
    let status = IoGetDeviceProperty(
        pdo,
        wdk_sys::DEVICE_REGISTRY_PROPERTY::DevicePropertyCompatibleIDs,
        len,
        buf,
        &mut len,
    );
    if !nt_success(status) {
        ExFreePool(buf);
        return None;
    }
    let result = parse_compat_ids(buf as *const u16, len as usize);
    ExFreePool(buf);
    result
}

unsafe fn parse_compat_ids(wide: *const u16, len_bytes: usize) -> Option<(u32, u8)> {
    let n = len_bytes / 2; // u16 units
    let mut i = 0;
    while i < n {
        // Find a string beginning with "c0c0_".
        if i + 14 < n
            && wide.add(i).read() == b'c' as u16
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
            // role char follows the 8 hex digits directly (`c0c0_<hex8><A|B>`)
            let role = wide.add(i + 5 + 8).read() as u8;
            let idx = match role {
                b'A' => 0u8,
                b'B' => 1u8,
                _ => return None,
            };
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

/// Resolve the bus control device interface to a device object.
/// Returns `(file_obj, dev_obj)`; the caller must `ObfDereferenceObject` the file.
unsafe fn open_bus_device() -> Option<(*mut wdk_sys::FILE_OBJECT, PDEVICE_OBJECT)> {
    let mut iface: PZZWSTR = ptr::null_mut();
    let guid = crate::comname::bus_guid();
    let status = IoGetDeviceInterfaces(
        &guid as *const wdk_sys::GUID,
        ptr::null_mut(),
        0,
        &mut iface,
    );
    if !nt_success(status) || iface.is_null() || *iface == 0 {
        return None;
    }

    let mut iface_len = 0usize;
    while *iface.add(iface_len) != 0 {
        iface_len += 1;
    }
    let mut iface_name = UNICODE_STRING {
        Length: (iface_len * core::mem::size_of::<u16>()) as u16,
        MaximumLength: ((iface_len + 1) * core::mem::size_of::<u16>()) as u16,
        Buffer: iface,
    };
    let mut file_obj: *mut wdk_sys::FILE_OBJECT = ptr::null_mut();
    let mut dev_obj: PDEVICE_OBJECT = ptr::null_mut();
    let status = IoGetDeviceObjectPointer(
        &mut iface_name,
        wdk_sys::FILE_READ_DATA,
        &mut file_obj,
        &mut dev_obj,
    );
    ExFreePool(iface as wdk_sys::PVOID);
    if !nt_success(status) || file_obj.is_null() || dev_obj.is_null() {
        return None;
    }
    Some((file_obj, dev_obj))
}

/// Send a synchronous METHOD_BUFFERED control request to the bus.
unsafe fn send_bus_control(
    dev_obj: PDEVICE_OBJECT,
    ioctl: u32,
    in_ptr: *mut core::ffi::c_void,
    in_len: u32,
    out_ptr: *mut core::ffi::c_void,
    out_len: u32,
) -> NTSTATUS {
    let mut event = core::mem::zeroed::<wdk_sys::KEVENT>();
    KeInitializeEvent(&mut event, wdk_sys::_EVENT_TYPE::NotificationEvent, 0);
    let mut iosb = core::mem::zeroed::<IO_STATUS_BLOCK>();

    let irp = IoBuildDeviceIoControlRequest(
        ioctl,
        dev_obj,
        in_ptr,
        in_len,
        out_ptr,
        out_len,
        0, // InternalDeviceControl = FALSE (IRP_MJ_DEVICE_CONTROL)
        &mut event as *mut wdk_sys::KEVENT,
        &mut iosb as *mut IO_STATUS_BLOCK,
    );
    if irp.is_null() {
        return wdk_sys::STATUS_INSUFFICIENT_RESOURCES;
    }
    let status = IofCallDriver(dev_obj, irp);
    if status == wdk_sys::STATUS_PENDING {
        let _ = KeWaitForSingleObject(
            &mut event as *mut wdk_sys::KEVENT as *mut core::ffi::c_void,
            wdk_sys::_KWAIT_REASON::Executive,
            wdk_sys::_MODE::KernelMode as i8,
            0,
            ptr::null_mut(),
        );
    }
    iosb.__bindgen_anon_1.Status
}

/// Open the bus control device interface and issue PORT_LINK to fetch the
/// shared `PairContext` VA.
unsafe fn link_to_bus(pair_id: u32, port_index: u8) -> u64 {
    let Some((file_obj, dev_obj)) = open_bus_device() else {
        return 0;
    };

    let mut in_buf = PortLinkRequest {
        pair_id,
        port_index,
    };
    let mut out_buf = PortLinkResponse { pair_va: 0 };
    let status = send_bus_control(
        dev_obj,
        IOCTL_COM0COM_PORT_LINK,
        &mut in_buf as *mut _ as *mut core::ffi::c_void,
        core::mem::size_of::<PortLinkRequest>() as u32,
        &mut out_buf as *mut _ as *mut core::ffi::c_void,
        core::mem::size_of::<PortLinkResponse>() as u32,
    );
    ObfDereferenceObject(file_obj as wdk_sys::PVOID);
    if nt_success(status) {
        out_buf.pair_va
    } else {
        0
    }
}

/// Drop the PORT_LINK reference. Called on device removal; the bus frees the
/// `PairContext` and its slot once both ports have unlinked.
pub(crate) unsafe fn unlink_from_bus(ext: *mut PortExtension) {
    let pair_va = (*ext).pair_va;
    if pair_va == 0 {
        return;
    }
    // 先断开本端的指针，再让总线释放上下文。总线在最后一个 UNLINK 到达时
    // 会立即 ExFreePool，若此处仍保留旧指针，移除过程中的读写/取消会把
    // 已释放的内存当成 PairContext 使用。
    (*ext).pair_va = 0;
    let Some((file_obj, dev_obj)) = open_bus_device() else {
        return;
    };
    let mut in_buf = PortUnlinkRequest {
        pair_id: (*ext).pair_id,
        port_index: (*ext).port_index,
        pair_va,
    };
    let _ = send_bus_control(
        dev_obj,
        IOCTL_COM0COM_PORT_UNLINK,
        &mut in_buf as *mut _ as *mut core::ffi::c_void,
        core::mem::size_of::<PortUnlinkRequest>() as u32,
        ptr::null_mut(),
        0,
    );
    ObfDereferenceObject(file_obj as wdk_sys::PVOID);
}
