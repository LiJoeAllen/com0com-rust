//! Bus FDO I/O dispatch: handles the com0com private IOCTLs.
//!
//! Issued by:
//!   * the user-mode `setupc` tool — CREATE/DELETE/ENUM/GET/SET pair params,
//!   * the `com0comport` driver — PORT_LINK / PORT_UNLINK.

use core::ptr;

use wdk_sys::{
    c_void, call_unsafe_wdf_function_binding, nt_success, NTSTATUS, WDFQUEUE, WDFREQUEST,
};

use crate::bus::{
    self, alloc_pair_context, alloc_pair_slot, find_slot, free_pair_context, free_pair_slot,
    raw_lock, raw_unlock, BusState, Com0comChildId, PairSlot,
};
use com0com_common::{
    ioctls,
    protocol::{
        CreatePairRequest, DeletePairRequest, EnumPairsResponse, GetParamsRequest, PortLinkRequest,
        PortLinkResponse, PortUnlinkRequest, SetParamsRequest,
    },
};

unsafe extern "C" fn evt_io_device_control(
    _queue: WDFQUEUE,
    request: WDFREQUEST,
    _input_length: usize,
    _output_length: usize,
    ioctl_code: u32,
) {
    let (status, info) = dispatch(request, ioctl_code);
    call_unsafe_wdf_function_binding!(WdfRequestCompleteWithInformation, request, status, info);
}

unsafe fn dispatch(request: WDFREQUEST, ioctl_code: u32) -> (NTSTATUS, usize) {
    match ioctl_code {
        ioctls::IOCTL_COM0COM_CREATE_PAIR => create_pair(request),
        ioctls::IOCTL_COM0COM_DELETE_PAIR => delete_pair(request),
        ioctls::IOCTL_COM0COM_ENUM_PAIRS => enum_pairs(request),
        ioctls::IOCTL_COM0COM_GET_PARAMS => get_params(request),
        ioctls::IOCTL_COM0COM_SET_PARAMS => set_params(request),
        ioctls::IOCTL_COM0COM_PORT_LINK => port_link(request),
        ioctls::IOCTL_COM0COM_PORT_UNLINK => port_unlink(request),
        _ => (wdk_sys::STATUS_INVALID_DEVICE_REQUEST, 0),
    }
}

fn retrieve_buffers<'a>(
    request: WDFREQUEST,
    min_in: usize,
    min_out: usize,
) -> Option<(&'a mut [u8], &'a mut [u8])> {
    unsafe {
        let mut in_buf: *mut c_void = ptr::null_mut();
        let mut out_buf: *mut c_void = ptr::null_mut();
        // VERIFY: the `*_Retrieve*Buffer` binding signature in your wdk-sys.
        let s1 = call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveInputBuffer,
            request,
            min_in,
            &mut in_buf,
            ptr::null_mut()
        );
        let s2 = call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputBuffer,
            request,
            min_out,
            &mut out_buf,
            ptr::null_mut()
        );
        if !nt_success(s1) || !nt_success(s2) || in_buf.is_null() || out_buf.is_null() {
            return None;
        }
        let in_slice = core::slice::from_raw_parts_mut(in_buf as *mut u8, min_in);
        let out_slice = core::slice::from_raw_parts_mut(out_buf as *mut u8, min_out);
        Some((in_slice, out_slice))
    }
}

unsafe fn create_pair(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, outb)) = retrieve_buffers(request, core::mem::size_of::<CreatePairRequest>(), 0)
    else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: CreatePairRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };

    let irql = raw_lock(&mut state.lock as *mut _);
    // Allocate a slot.
    let mut slot_idx: Option<usize> = None;
    for i in 0..bus::MAX_PAIRS {
        if state.slots[i].is_null() {
            slot_idx = Some(i);
            break;
        }
    }
    let idx = match slot_idx {
        Some(i) => i,
        None => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INSUFFICIENT_RESOURCES, 0);
        }
    };

    let pair_va = alloc_pair_context(req.pair_id);
    if pair_va.is_null() {
        raw_unlock(&mut state.lock as *mut _, irql);
        return (wdk_sys::STATUS_INSUFFICIENT_RESOURCES, 0);
    }
    let slot = alloc_pair_slot();
    if slot.is_null() {
        free_pair_context(pair_va);
        raw_unlock(&mut state.lock as *mut _, irql);
        return (wdk_sys::STATUS_INSUFFICIENT_RESOURCES, 0);
    }
    (*slot).pair_id = req.pair_id;
    (*slot).pair_va = pair_va as u64;
    (*slot).params = req.params;
    (*slot).child_present = [false, false];
    (*slot).ref_count = 0;
    state.slots[idx] = slot;
    raw_unlock(&mut state.lock as *mut _, irql);

    // Enumerate the two port PDOs.
    for port_index in 0..2u8 {
        let mut id = Com0comChildId {
            header: wdk_sys::WDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER::default(),
            pair_id: req.pair_id,
            port_index,
        };
        id.header.Size = core::mem::size_of::<Com0comChildId>() as u32;
        let s = call_unsafe_wdf_function_binding!(
            WdfChildListAddOrUpdateChildDescriptionAsPresent,
            state.child_list,
            &mut id as *mut Com0comChildId
                as *mut wdk_sys::WDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER,
            ptr::null_mut()
        );
        if nt_success(s) {
            (*slot).child_present[port_index as usize] = true;
        }
    }

    (wdk_sys::STATUS_SUCCESS, 0)
}

unsafe fn delete_pair(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, _)) = retrieve_buffers(request, core::mem::size_of::<DeletePairRequest>(), 0)
    else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: DeletePairRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };

    let irql = raw_lock(&mut state.lock as *mut _);
    let slot = match find_slot(state, req.pair_id) {
        Some(s) => s,
        None => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    // Request teardown: pull both children from the bus. The PairContext is
    // freed when both ports have issued PORT_UNLINK (ref_count -> 0).
    (*slot).child_present = [false, false];
    let mut id = Com0comChildId {
        header: wdk_sys::WDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER::default(),
        pair_id: req.pair_id,
        port_index: 0,
    };
    id.header.Size = core::mem::size_of::<Com0comChildId>() as u32;
    for port_index in 0..2u8 {
        id.port_index = port_index;
        let _ = call_unsafe_wdf_function_binding!(
            WdfChildListUpdateChildDescriptionAsMissing,
            state.child_list,
            &mut id as *mut Com0comChildId
                as *mut wdk_sys::WDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER
        );
    }
    raw_unlock(&mut state.lock as *mut _, irql);
    (wdk_sys::STATUS_SUCCESS, 0)
}

unsafe fn enum_pairs(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((_, outb)) = retrieve_buffers(request, 0, core::mem::size_of::<EnumPairsResponse>())
    else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };
    let irql = raw_lock(&mut state.lock as *mut _);
    let mut resp = EnumPairsResponse::default();
    let mut count = 0u32;
    for i in 0..bus::MAX_PAIRS {
        let slot = state.slots[i];
        if !slot.is_null() {
            resp.entries[count as usize] = com0com_common::protocol::EnumPairEntry {
                pair_id: (*slot).pair_id,
                params: (*slot).params,
            };
            count += 1;
        }
    }
    resp.count = count;
    raw_unlock(&mut state.lock as *mut _, irql);
    write_struct(outb, &resp);
    (
        wdk_sys::STATUS_SUCCESS,
        core::mem::size_of::<EnumPairsResponse>(),
    )
}

unsafe fn get_params(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, outb)) = retrieve_buffers(
        request,
        core::mem::size_of::<GetParamsRequest>(),
        core::mem::size_of::<com0com_common::protocol::Com0comPairParams>(),
    ) else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: GetParamsRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };
    let irql = raw_lock(&mut state.lock as *mut _);
    let slot = match find_slot(state, req.pair_id) {
        Some(s) => s,
        None => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    let params = (*slot).params;
    raw_unlock(&mut state.lock as *mut _, irql);
    write_struct(outb, &params);
    (
        wdk_sys::STATUS_SUCCESS,
        core::mem::size_of::<com0com_common::protocol::Com0comPairParams>(),
    )
}

unsafe fn set_params(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, _)) = retrieve_buffers(request, core::mem::size_of::<SetParamsRequest>(), 0)
    else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: SetParamsRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };
    let irql = raw_lock(&mut state.lock as *mut _);
    let slot = match find_slot(state, req.pair_id) {
        Some(s) => s,
        None => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    (*slot).params = req.params;
    raw_unlock(&mut state.lock as *mut _, irql);
    (wdk_sys::STATUS_SUCCESS, 0)
}

unsafe fn port_link(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, outb)) = retrieve_buffers(
        request,
        core::mem::size_of::<PortLinkRequest>(),
        core::mem::size_of::<PortLinkResponse>(),
    ) else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: PortLinkRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };
    let irql = raw_lock(&mut state.lock as *mut _);
    let slot = match find_slot(state, req.pair_id) {
        Some(s) => s,
        None => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    (*slot).ref_count += 1;
    let pair_va = (*slot).pair_va;
    raw_unlock(&mut state.lock as *mut _, irql);

    let resp = PortLinkResponse { pair_va };
    write_struct(outb, &resp);
    (
        wdk_sys::STATUS_SUCCESS,
        core::mem::size_of::<PortLinkResponse>(),
    )
}

unsafe fn port_unlink(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, _)) = retrieve_buffers(request, core::mem::size_of::<PortUnlinkRequest>(), 0)
    else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: PortUnlinkRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };
    let irql = raw_lock(&mut state.lock as *mut _);
    let slot = match find_slot(state, req.pair_id) {
        Some(s) => s,
        None => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    (*slot).ref_count -= 1;
    if (*slot).ref_count <= 0 && (*slot).child_present == [false, false] {
        let pair_va = (*slot).pair_va;
        let idx = state.slots.iter().position(|&p| p == slot).unwrap_or(0);
        state.slots[idx] = ptr::null_mut();
        raw_unlock(&mut state.lock as *mut _, irql);
        free_pair_context(pair_va as *mut _);
        free_pair_slot(slot);
    } else {
        raw_unlock(&mut state.lock as *mut _, irql);
    }
    (wdk_sys::STATUS_SUCCESS, 0)
}

/// Copy a `repr(C)` struct out of a byte slice.
unsafe fn read_struct<T: Copy>(buf: &[u8]) -> T {
    let mut val = core::mem::MaybeUninit::<T>::uninit();
    let dst = val.as_mut_ptr() as *mut u8;
    let n = core::mem::size_of::<T>().min(buf.len());
    ptr::copy_nonoverlapping(buf.as_ptr(), dst, n);
    val.assume_init()
}

/// Copy a `repr(C)` struct into a byte slice.
unsafe fn write_struct<T: Copy>(buf: &mut [u8], val: &T) {
    let src = val as *const T as *const u8;
    let n = core::mem::size_of::<T>().min(buf.len());
    ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), n);
}
