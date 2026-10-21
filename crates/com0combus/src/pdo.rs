//! Child PDO creation callback for the virtual port devices.
//!
//! The bus enumerates one PDO per port. Each PDO is given:
//!   * a hardware ID of `COM0COM\PORT_A` or `COM0COM\PORT_B` (matched by the
//!     port INF so `com0comport.sys` loads as the function driver), and
//!   * an instance ID encoding the pair id + port role, which the port driver
//!     reads back to locate its `PairContext`.

use core::ptr;

use wdk::nt_success;
use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, PWDFDEVICE_INIT,
    PWDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER, UNICODE_STRING, WDFCHILDLIST, WDFDEVICE,
};

use crate::bus::Com0comChildId;
use com0com_common::constants::{PORT_A_HW_ID, PORT_B_HW_ID};
use com0com_common::naming::{build_compat_id, build_instance_id};

/// Encode ASCII `ascii` as UTF-16 into `buf`, returning a `UNICODE_STRING`
/// whose `Buffer` points at `buf`. `buf` must outlive the returned struct.
unsafe fn ascii_to_unicode(ascii: &[u8], buf: &mut [u16]) -> UNICODE_STRING {
    let n = com0com_common::naming::ascii_to_unicode_bytes(ascii, buf);
    UNICODE_STRING {
        Length: ((n - 1) * 2) as u16,
        MaximumLength: (buf.len() * 2) as u16,
        Buffer: buf.as_mut_ptr(),
    }
}

pub(crate) unsafe extern "C" fn evt_child_list_create_device(
    _child_list: WDFCHILDLIST,
    identification_description: PWDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER,
    mut device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    if identification_description.is_null() {
        return wdk_sys::STATUS_INVALID_PARAMETER;
    }

    // The framework passes the exact child-description bytes supplied to
    // WdfChildListAddOrUpdateChildDescriptionAsPresent.
    let id = &*(identification_description as *const Com0comChildId);

    let hw_id = if id.port_index == 0 {
        PORT_A_HW_ID
    } else {
        PORT_B_HW_ID
    };
    let mut hw_buf = [0u16; 64];
    let hw_uni = ascii_to_unicode(hw_id.as_bytes(), &mut hw_buf);
    let status = call_unsafe_wdf_function_binding!(
        WdfPdoInitAddHardwareID,
        device_init,
        &hw_uni as *const UNICODE_STRING
    );
    if !nt_success(status) {
        return status;
    }

    // Compatible ID encodes (pair_id, port_role). It is retrievable from the
    // WDM port driver via IoGetDeviceProperty(DevicePropertyCompatibleIDs) and
    // is NOT used by the INF for matching, so it never affects installation.
    let mut compat = [0u8; 48];
    build_compat_id(&mut compat, id.pair_id, id.port_index);
    let mut compat_buf = [0u16; 64];
    let compat_uni = ascii_to_unicode(&compat, &mut compat_buf);
    let status = call_unsafe_wdf_function_binding!(
        WdfPdoInitAddCompatibleID,
        device_init,
        &compat_uni as *const UNICODE_STRING
    );
    if !nt_success(status) {
        return status;
    }

    let mut inst = [0u8; 32];
    build_instance_id(
        &mut inst,
        id.pair_id,
        if id.port_index == 0 { b'A' } else { b'B' },
    );
    let mut inst_buf = [0u16; 64];
    let inst_uni = ascii_to_unicode(&inst, &mut inst_buf);
    let status = call_unsafe_wdf_function_binding!(
        WdfPdoInitAssignInstanceID,
        device_init,
        &inst_uni as *const UNICODE_STRING
    );
    if !nt_success(status) {
        return status;
    }

    let mut pdo: WDFDEVICE = ptr::null_mut();
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreate,
        &mut device_init,
        wdk_sys::WDF_NO_OBJECT_ATTRIBUTES,
        &mut pdo
    );
    if !nt_success(status) {
        return status;
    }

    wdk_sys::STATUS_SUCCESS
}
