//! DriverEntry and bus FDO creation (KMDF).

use core::ptr;

use wdk_sys::{
    call_unsafe_wdf_function_binding, nt_success, DRIVER_OBJECT, NTSTATUS, PCUNICODE_STRING,
    PDRIVER_OBJECT, WDFDEVICE, WDFDEVICE_INIT, WDFDRIVER, WDF_CHILD_LIST_CONFIG, WDF_DRIVER_CONFIG,
    WDF_DRIVER_CONFIG_SIZE, WDF_IO_QUEUE_CONFIG, WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES,
    WDF_NO_STRING,
};

use crate::{
    bus::{self, guid_from_bytes, raw_lock, raw_unlock, BusState, Com0comChildId},
    ioctl, pdo,
};

#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut driver_config = WDF_DRIVER_CONFIG {
        Size: WDF_DRIVER_CONFIG_SIZE,
        EvtDriverDeviceAdd: Some(evt_driver_device_add),
        ..WDF_DRIVER_CONFIG::default()
    };
    let mut driver_handle = WDF_NO_HANDLE;
    let status = call_unsafe_wdf_function_binding!(
        WdfDriverCreate,
        driver,
        registry_path,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut driver_config,
        &mut driver_handle
    );
    if !nt_success(status) {
        return status;
    }
    wdk_sys::STATUS_SUCCESS
}

unsafe extern "C" fn evt_driver_device_add(
    _driver: WDFDRIVER,
    device_init: *mut WDFDEVICE_INIT,
) -> NTSTATUS {
    let mut device: WDFDEVICE = WDF_NO_HANDLE;
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreate,
        &mut device_init,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut device
    );
    if !nt_success(status) {
        return status;
    }

    // Default I/O queue: all major functions land in our dispatcher. The port
    // driver and setupc send private IOCTLs here.
    let mut queue_config = WDF_IO_QUEUE_CONFIG::default();
    queue_config.Size = core::mem::size_of::<WDF_IO_QUEUE_CONFIG>() as u32;
    queue_config.EvtIoDeviceControl = Some(ioctl::evt_io_device_control);
    let mut queue = WDF_NO_HANDLE;
    let status = call_unsafe_wdf_function_binding!(
        WdfIoQueueCreate,
        device,
        &mut queue_config,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut queue
    );
    if !nt_success(status) {
        return status;
    }

    // Static child list for the virtual port PDOs.
    let mut child_config = WDF_CHILD_LIST_CONFIG::default();
    child_config.Size = core::mem::size_of::<WDF_CHILD_LIST_CONFIG>() as u32;
    child_config.EvtChildListCreateDevice = Some(pdo::evt_child_list_create_device);
    child_config.IdentificationDescriptionType.Size = core::mem::size_of::<Com0comChildId>() as u32;
    let mut child_list = WDF_NO_HANDLE;
    let status = call_unsafe_wdf_function_binding!(
        WdfChildListCreate,
        device,
        &mut child_config,
        &mut child_list
    );
    if !nt_success(status) {
        return status;
    }

    // Control device interface (setupc + port driver talk here).
    let bus_guid = guid_from_bytes(&bus::GUID_COM0COM_BUS_BYTES);
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreateDeviceInterface,
        device,
        &bus_guid,
        WDF_NO_STRING
    );
    if !nt_success(status) {
        return status;
    }

    // Allocate and publish singleton bus state.
    let state = bus::alloc_bus_state(device, child_list);
    if state.is_null() {
        return wdk_sys::STATUS_INSUFFICIENT_RESOURCES;
    }
    bus::set_bus(state);

    wdk_sys::STATUS_SUCCESS
}
