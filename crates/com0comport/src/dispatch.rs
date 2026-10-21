//! WDM major-function dispatch table entry points.

use wdk_sys::{
    ntddk::{
        IoDeleteDevice, IoDetachDevice, IofCallDriver as IoCallDriver,
        IofCompleteRequest as IoCompleteRequest,
    },
    IO_NO_INCREMENT, IRP_MN_REMOVE_DEVICE, NTSTATUS, PDEVICE_OBJECT, PIRP,
};

use crate::{
    comname,
    driver::{
        copy_current_irp_stack_location_to_next, current_irp_stack_location,
        set_completion_routine, skip_current_irp_stack_location, PortExtension,
    },
    readwrite, serial,
};

pub(crate) unsafe extern "C" fn dispatch_create_close(
    device: PDEVICE_OBJECT,
    irp: PIRP,
) -> NTSTATUS {
    let _ext = (*device).DeviceExtension as *mut PortExtension;
    (*irp).IoStatus.Information = 0;
    (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
    IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
    wdk_sys::STATUS_SUCCESS
}

pub(crate) unsafe extern "C" fn dispatch_cleanup(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    readwrite::cancel_all(ext);
    (*irp).IoStatus.Information = 0;
    (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
    IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
    wdk_sys::STATUS_SUCCESS
}

pub(crate) unsafe extern "C" fn dispatch_read(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    readwrite::handle_read(ext, irp)
}

pub(crate) unsafe extern "C" fn dispatch_write(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    readwrite::handle_write(ext, irp)
}

pub(crate) unsafe extern "C" fn dispatch_device_control(
    device: PDEVICE_OBJECT,
    irp: PIRP,
) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    serial::dispatch_serial(ext, irp)
}

pub(crate) unsafe extern "C" fn dispatch_pnp(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    let stack = current_irp_stack_location(irp);
    let minor = (*stack).MinorFunction;
    if u32::from(minor) == IRP_MN_REMOVE_DEVICE {
        copy_current_irp_stack_location_to_next(irp);
        set_completion_routine(
            irp,
            Some(on_remove_complete),
            device as *mut core::ffi::c_void,
            true,
            true,
            true,
        );
        return IoCallDriver((*ext).lower_device, irp);
    }
    skip_current_irp_stack_location(irp);
    IoCallDriver((*ext).lower_device, irp)
}

pub(crate) unsafe extern "C" fn dispatch_power(device: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    skip_current_irp_stack_location(irp);
    IoCallDriver((*ext).lower_device, irp)
}

pub(crate) unsafe extern "C" fn dispatch_system_control(
    device: PDEVICE_OBJECT,
    irp: PIRP,
) -> NTSTATUS {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    skip_current_irp_stack_location(irp);
    IoCallDriver((*ext).lower_device, irp)
}

unsafe extern "C" fn on_remove_complete(
    _lower_device: PDEVICE_OBJECT,
    _irp: PIRP,
    context: *mut core::ffi::c_void,
) -> NTSTATUS {
    let device = context as PDEVICE_OBJECT;
    let ext = (*device).DeviceExtension as *mut PortExtension;
    let lower = (*ext).lower_device;
    if !lower.is_null() {
        IoDetachDevice(lower);
    }
    comname::unregister(ext);
    readwrite::cancel_all(ext);
    IoDeleteDevice(device);
    // Let the IRP propagate back up.
    wdk_sys::STATUS_SUCCESS
}
