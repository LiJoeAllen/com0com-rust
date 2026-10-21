//! DriverEntry and bus FDO creation (KMDF).

use core::ptr;

use com0com_common::constants::GUID_COM0COM_BUS_BYTES;
use wdk::nt_success;
use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, WDFCHILDLIST,
    WDFDEVICE, WDFDEVICE_INIT, WDFDRIVER, WDFQUEUE, WDF_CHILD_LIST_CONFIG, WDF_DRIVER_CONFIG,
    WDF_IO_QUEUE_CONFIG, WDF_NO_OBJECT_ATTRIBUTES,
};

use crate::{
    bus::{self, guid_from_bytes, Com0comChildId},
    ioctl, pdo,
};

/// # Safety
/// Invoked by the I/O manager with a valid `DRIVER_OBJECT`; never call directly.
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut driver_config = WDF_DRIVER_CONFIG {
        Size: core::mem::size_of::<WDF_DRIVER_CONFIG>() as u32,
        EvtDriverDeviceAdd: Some(evt_driver_device_add),
        ..WDF_DRIVER_CONFIG::default()
    };
    let mut driver_handle: WDFDRIVER = ptr::null_mut();
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
    mut device_init: *mut WDFDEVICE_INIT,
) -> NTSTATUS {
    let mut device: WDFDEVICE = ptr::null_mut();
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreate,
        &mut device_init,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut device
    );
    if !nt_success(status) {
        return status;
    }
    klog!("[c0cbus] device created");

    // Default I/O queue: all major functions land in our dispatcher. The port
    // driver and setupc send private IOCTLs here. The queue must not be
    // power-managed: this bus has no power policy and its requests must be
    // dispatched regardless of the FDO's power state.
    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: core::mem::size_of::<WDF_IO_QUEUE_CONFIG>() as u32,
        DispatchType: wdk_sys::_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        PowerManaged: wdk_sys::_WDF_TRI_STATE::WdfFalse,
        AllowZeroLengthRequests: 1,
        DefaultQueue: 1,
        EvtIoDefault: Some(ioctl::evt_io_default),
        EvtIoDeviceControl: Some(ioctl::evt_io_device_control),
        // Parallel 队列的并发上限。零初始化会被 KMDF 当作“最多同时呈现 0 个请求”：
        // 请求只会入队，永远不会回调驱动。官方 INIT 宏在 Parallel 模式下把它设为
        // (ULONG)-1，这里必须显式照做，否则表现为所有 I/O 永久挂起。
        Settings: wdk_sys::_WDF_IO_QUEUE_CONFIG__bindgen_ty_1 {
            Parallel: wdk_sys::_WDF_IO_QUEUE_CONFIG__bindgen_ty_1__bindgen_ty_1 {
                NumberOfPresentedRequests: u32::MAX,
            },
        },
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    let mut queue: WDFQUEUE = ptr::null_mut();
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
    klog!("[c0cbus] queue created status=%lx", status as u32);

    // Static child list for the virtual port PDOs.
    let mut child_config = WDF_CHILD_LIST_CONFIG {
        Size: core::mem::size_of::<WDF_CHILD_LIST_CONFIG>() as u32,
        EvtChildListCreateDevice: Some(pdo::evt_child_list_create_device),
        IdentificationDescriptionSize: core::mem::size_of::<Com0comChildId>() as u32,
        ..WDF_CHILD_LIST_CONFIG::default()
    };
    let mut child_list: WDFCHILDLIST = ptr::null_mut();
    let status = call_unsafe_wdf_function_binding!(
        WdfChildListCreate,
        device,
        &mut child_config,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut child_list
    );
    if !nt_success(status) {
        return status;
    }
    klog!("[c0cbus] child list created status=%lx", status as u32);

    // Control device interface (setupc + port driver talk here).
    let bus_guid = guid_from_bytes(&GUID_COM0COM_BUS_BYTES);
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreateDeviceInterface,
        device,
        &bus_guid,
        ptr::null()
    );
    if !nt_success(status) {
        return status;
    }
    klog!("[c0cbus] interface created status=%lx", status as u32);

    // Allocate and publish singleton bus state.
    let state = bus::alloc_bus_state(device, child_list);
    if state.is_null() {
        return wdk_sys::STATUS_INSUFFICIENT_RESOURCES;
    }
    bus::set_bus(state);

    wdk_sys::STATUS_SUCCESS
}
