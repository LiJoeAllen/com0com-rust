//! Bus FDO I/O dispatch: handles the com0com private IOCTLs.
//!
//! Issued by:
//!   * the user-mode `setupc` tool — CREATE/DELETE/ENUM/GET/SET pair params,
//!   * the `com0comport` driver — PORT_LINK / PORT_UNLINK.

use core::{ffi::c_void, ptr};

use wdk::nt_success;
use wdk_sys::{call_unsafe_wdf_function_binding, KSPIN_LOCK, NTSTATUS, WDFQUEUE, WDFREQUEST};

use crate::bus::{
    self, alloc_pair_context, alloc_pair_slot, find_slot, free_pair_context, free_pair_slot,
    raw_lock, raw_unlock, Com0comChildId,
};
use com0com_common::{
    ioctls,
    pair::PairContext,
    protocol::{
        CreatePairRequest, DeletePairRequest, EnumPairEntry, EnumPairsResponse, GetParamsRequest,
        PortLinkRequest, PortLinkResponse, PortUnlinkRequest, SetParamsRequest, MAX_ENUM_PAIRS,
    },
};

pub(crate) unsafe extern "C" fn evt_io_device_control(
    _queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_length: usize,
    _input_length: usize,
    ioctl_code: u32,
) {
    let (status, info) = dispatch(request, ioctl_code);
    call_unsafe_wdf_function_binding!(
        WdfRequestCompleteWithInformation,
        request,
        status,
        info as u64
    );
}

/// 兜底回调：默认队列里除 IOCTL 之外的请求（读/写等）本驱动不支持，直接失败。
pub(crate) unsafe extern "C" fn evt_io_default(_queue: WDFQUEUE, request: WDFREQUEST) {
    call_unsafe_wdf_function_binding!(
        WdfRequestCompleteWithInformation,
        request,
        wdk_sys::STATUS_INVALID_DEVICE_REQUEST,
        0u64
    );
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
        // 零长度方向不向 WDF 取缓冲区：此时 WDF 返回的 Buffer 可能为 NULL，
        // 对 NULL 构造切片是未定义行为。按 IOCTL 定义直接给空切片。
        let s1 = if min_in == 0 {
            wdk_sys::STATUS_SUCCESS
        } else {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveInputBuffer,
                request,
                min_in,
                &mut in_buf,
                ptr::null_mut()
            )
        };
        let s2 = if min_out == 0 {
            wdk_sys::STATUS_SUCCESS
        } else {
            call_unsafe_wdf_function_binding!(
                WdfRequestRetrieveOutputBuffer,
                request,
                min_out,
                &mut out_buf,
                ptr::null_mut()
            )
        };
        if !nt_success(s1) || !nt_success(s2) {
            return None;
        }
        if (min_in > 0 && in_buf.is_null()) || (min_out > 0 && out_buf.is_null()) {
            return None;
        }
        let in_slice: &mut [u8] = if min_in == 0 {
            &mut []
        } else {
            core::slice::from_raw_parts_mut(in_buf as *mut u8, min_in)
        };
        let out_slice: &mut [u8] = if min_out == 0 {
            &mut []
        } else {
            core::slice::from_raw_parts_mut(out_buf as *mut u8, min_out)
        };
        Some((in_slice, out_slice))
    }
}

unsafe fn create_pair(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let Some((inb, _outb)) =
        retrieve_buffers(request, core::mem::size_of::<CreatePairRequest>(), 0)
    else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let req: CreatePairRequest = read_struct(inb);
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };

    let irql = raw_lock(&mut state.lock as *mut _);
    // 重复 id 会让 find_slot 命中旧槽，后续 update/remove 会作用到错误的 pair。
    if find_slot(state, req.pair_id).is_some() {
        raw_unlock(&mut state.lock as *mut _, irql);
        return (wdk_sys::STATUS_OBJECT_NAME_COLLISION, 0);
    }
    let idx = match bus::alloc_slot(&mut *state) {
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
    (*slot).deleted = false;
    (*slot).ref_count = 0;
    // Mirror the params into the shared PairContext: the *port* driver never
    // sees the bus slot, so this is the only channel through which `emu_br` /
    // `emu_noise` / `emu_overrun` / `noise_probability` reach the data path.
    // Done while `state.lock` is held and before the PDOs are enumerated.
    (*pair_va).params = req.params;
    state.slots[idx] = slot;

    // Enumerate the two port PDOs while still holding the bus lock: a parallel
    // DELETE_PAIR could otherwise mark the pair missing and we would re-add the
    // children afterwards (or free the slot out from under us). Both child-list
    // APIs are documented as callable at IRQL <= DISPATCH_LEVEL, so the raised
    // IRQL from the spinlock is fine.
    for port_index in 0..2u8 {
        // zeroed(): 填充字节也参与 KMDF 的 RtlCompareMemory 比较，
        // 结构体字面量会留下未初始化的尾部填充，导致后续 remove 查找失败。
        let mut id: Com0comChildId = core::mem::zeroed();
        id.pair_id = req.pair_id;
        id.port_index = port_index;
        id.header.IdentificationDescriptionSize = core::mem::size_of::<Com0comChildId>() as u32;
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
    raw_unlock(&mut state.lock as *mut _, irql);

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
        Some(s) if !(*s).deleted => s,
        _ => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    // Mark first so ENUM_PAIRS hides the pair immediately and new PORT_LINK
    // requests are rejected, then pull both children from the child list. The
    // PairContext is freed when the last port issues PORT_UNLINK.
    (*slot).deleted = true;
    let present = (*slot).child_present;
    (*slot).child_present = [false, false];
    // 与 create_pair 一样必须零初始化：KMDF 按完整结构体字节比较。
    // 之前的结构体字面量留下了未初始化的尾部填充，RtlCompareMemory 因此
    // 匹配不到条目，UpdateAsMissing 一直返回 STATUS_NO_SUCH_DEVICE（被忽略），
    // 表现为 `setupc remove` 打印成功但 pair/串口仍然存在（check17b）。
    let mut id: Com0comChildId = core::mem::zeroed();
    id.pair_id = req.pair_id;
    id.header.IdentificationDescriptionSize = core::mem::size_of::<Com0comChildId>() as u32;
    let mut first_error = wdk_sys::STATUS_SUCCESS;
    for port_index in 0..2u8 {
        // 从未枚举成功的端口没有 child-list 条目，无需（也无法）标记 missing。
        if !present[port_index as usize] {
            continue;
        }
        id.port_index = port_index;
        let s = call_unsafe_wdf_function_binding!(
            WdfChildListUpdateChildDescriptionAsMissing,
            state.child_list,
            &mut id as *mut Com0comChildId
                as *mut wdk_sys::WDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER
        );
        if !nt_success(s) && s != wdk_sys::STATUS_NO_SUCH_DEVICE && nt_success(first_error) {
            first_error = s;
        }
    }

    // 没有任何端口持有引用时（例如端口驱动还没完成 PORT_LINK 就删除），
    // 不会再有 PORT_UNLINK 到来，在这里直接回收槽位与 PairContext。
    if (*slot).ref_count <= 0 {
        let pair_va = (*slot).pair_va;
        if let Some(idx) = state.slots.iter().position(|&p| p == slot) {
            state.slots[idx] = ptr::null_mut();
        }
        raw_unlock(&mut state.lock as *mut _, irql);
        free_pair_context(pair_va as *mut _);
        free_pair_slot(slot);
    } else {
        raw_unlock(&mut state.lock as *mut _, irql);
    }
    (first_error, 0)
}

unsafe fn enum_pairs(request: WDFREQUEST) -> (NTSTATUS, usize) {
    let size = core::mem::size_of::<EnumPairsResponse>();
    let Some((_, outb)) = retrieve_buffers(request, 0, size) else {
        return (wdk_sys::STATUS_BUFFER_TOO_SMALL, 0);
    };
    let state = match bus::bus() {
        Some(s) => s,
        None => return (wdk_sys::STATUS_DEVICE_NOT_READY, 0),
    };

    // EnumPairsResponse 约 10 KB，绝不可以在内核栈上构造：x64 内核栈只有
    // 12–24 KB，而本函数随后获取自旋锁把 IRQL 抬到 DISPATCH，栈增长时
    // `_chkstk` 探测新页会直接触发 0x0A 蓝屏（v7 实际崩溃点）。
    // METHOD_BUFFERED 的系统缓冲区是非分页内存，直接就地清零并填充。
    ptr::write_bytes(outb.as_mut_ptr(), 0, size);
    let resp = outb.as_mut_ptr() as *mut EnumPairsResponse;
    let entries = ptr::addr_of_mut!((*resp).entries) as *mut EnumPairEntry;

    let irql = raw_lock(&mut state.lock as *mut _);
    let mut count = 0usize;
    for slot in state
        .slots
        .iter()
        .copied()
        .filter(|&s| !s.is_null() && !(*s).deleted)
    {
        if count >= MAX_ENUM_PAIRS {
            break;
        }
        // 逐字段写入，避免构造大的栈上临时值。
        let entry = entries.add(count);
        (*entry).pair_id = (*slot).pair_id;
        (*entry).params = (*slot).params;
        count += 1;
    }
    (*resp).count = count as u32;
    raw_unlock(&mut state.lock as *mut _, irql);
    (wdk_sys::STATUS_SUCCESS, size)
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
        Some(s) if !(*s).deleted => s,
        _ => {
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
        Some(s) if !(*s).deleted => s,
        _ => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
        }
    };
    (*slot).params = req.params;
    // Keep the shared PairContext (and therefore both port drivers) in sync.
    // The pair VA is stable for the lifetime of the slot, and the slot cannot
    // be freed while we hold `state.lock`.
    //
    // The write itself must hold the *pair* lock: the port driver reads
    // `(*pc).params` under that same lock (com0comport::readwrite::handle_write),
    // and `Com0comPairParams` is 164 bytes — without the lock a port can observe
    // a torn update (e.g. emu_noise == 1 with a stale noise_probability == 0).
    // Lock order is bus -> pair everywhere; the port driver never takes the bus
    // lock, so this cannot deadlock.
    let pair_va = (*slot).pair_va as *mut PairContext;
    if !pair_va.is_null() {
        let pair_irql = raw_lock(&mut (*pair_va).lock as *mut u64 as *mut KSPIN_LOCK);
        (*pair_va).params = req.params;
        raw_unlock(
            &mut (*pair_va).lock as *mut u64 as *mut KSPIN_LOCK,
            pair_irql,
        );
    }
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
        Some(s) if !(*s).deleted => s,
        _ => {
            raw_unlock(&mut state.lock as *mut _, irql);
            return (wdk_sys::STATUS_NO_SUCH_DEVICE, 0);
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
    // 校验 VA：pair id 之后可能被复用，若旧端口的 UNLINK 迟到，
    // 不能让它在新的槽位上误减引用计数。
    if (*slot).pair_va != req.pair_va || (*slot).ref_count <= 0 {
        raw_unlock(&mut state.lock as *mut _, irql);
        return (wdk_sys::STATUS_INVALID_PARAMETER, 0);
    }
    (*slot).ref_count -= 1;
    if (*slot).ref_count <= 0 && (*slot).child_present == [false, false] {
        let pair_va = (*slot).pair_va;
        if let Some(idx) = state.slots.iter().position(|&p| p == slot) {
            state.slots[idx] = ptr::null_mut();
        }
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
