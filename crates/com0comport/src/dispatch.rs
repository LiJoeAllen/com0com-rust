//! WDM major-function dispatch table entry points.

use core::ptr;

use wdk_sys::{
    nt_success, IoCallDriver, IoCompleteRequest, IoDeleteDevice, IoDetachDevice,
    IoGetCurrentIrpStackLocation, IoSetCompletionRoutine, IoSkipCurrentIrpStackLocation,
    IO_NO_INCREMENT, IRP_MN_REMOVE_DEVICE, NTSTATUS, PDEVICE_OBJECT, PIRP,
};

use crate::{comname, driver::PortExtension, readwrite, serial};

unsafe extern "C" fn dispatch_create_close(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let _ext = (*device).DeviceExtension as *mut PortExtension;
    (*irp).IoStatus.Information = 0;
    (*irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    wdk_sys::STATUS_SUCCESS
}

unsafe extern "C" fn dispatch_cleanup(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    readwrite::cancel_all(ext);
    (*irp).IoStatus.Information = 0;
    (*irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    wdk_sys::STATUS_SUCCESS
}

unsafe extern "C" fn dispatch_read(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    readwrite::handle_read(ext, irp)
}

unsafe extern "C" fn dispatch_write(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    readwrite::handle_write(ext, irp)
}

unsafe extern "C" fn dispatch_device_control(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    serial::dispatch_serial(ext, irp)
}

unsafe extern "C" fn dispatch_pnp(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    let stack = IoGetCurrentIrpStackLocation(irp);
    let minor = (*stack).MinorFunction;
    if minor == IRP_MN_REMOVE_DEVICE {
        IoSetCompletionRoutine(irp, Some(on_remove_complete), ptr::null_mut(), 1, 1, 1);
        return IoCallDriver((*ext).lower_device, irp);
    }
    IoSkipCurrentIrpStackLocation(irp);
    IoCallDriver((*ext).lower_device, irp)
}

unsafe extern "C" fn dispatch_power(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    IoSkipCurrentIrpStackLocation(irp);
    IoCallDriver((*ext).lower_device, irp)
}

unsafe extern "C" fn dispatch_system_control(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    IoSkipCurrentIrpStackLocation(irp);
    IoCallDriver((*ext).lower_device, irp)
}

unsafe extern "C" fn on_remove_complete(
    device: PDEVICE_OBJECT,
    irp: PIRP,
    _context: *mut core::ffi::c_void,
) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    let lower = (*ext).lower_device;
    if !lower.is_null() {
        IoDetachDevice(lower);
    }
    comname::unregister(ext);
    readwrite::cancel_all(ext);
    wdk_sys::IoDeleteDevice(device);
    // Let the IRP propagate back up.
    nt_success((*irp).IoStatus.Status);
    wdk_sys::STATUS_SUCCESS
}
