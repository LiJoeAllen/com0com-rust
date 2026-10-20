//! Cross-port data path + cancel-safe pending read/event IRPs.
//!
//! A write to port X copies into the *peer* port's RX ring buffer. If the peer
//! has a pended read (or a pended WAIT_ON_MASK), it is satisfied immediately.
//! All shared-state access is guarded by the `PairContext` spinlock.

use wdk_sys::{
    IoCompleteRequest, IoGetCurrentIrpStackLocation, IoSetCancelRoutine, KeAcquireSpinLock,
    KeReleaseSpinLock, IO_NO_INCREMENT, KIRQL, KSPIN_LOCK, NTSTATUS, PDEVICE_OBJECT, PIRP,
    SERIAL_EV_RXCHAR,
};

use com0com_common::pair::PairContext;

use crate::driver::PortExtension;

unsafe fn lock_pair(pc: *mut PairContext) -> KIRQL {
    let mut irql: KIRQL = 0;
    KeAcquireSpinLock(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK, &mut irql);
    irql
}

#[inline]
unsafe fn unlock_pair(pc: *mut PairContext, irql: KIRQL) {
    KeReleaseSpinLock(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK, irql);
}

pub unsafe fn handle_read(ext: *mut PortExtension, irp: PIRP) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        (*irp).IoStatus.Information = 0;
        (*irp).IoStatus.Status = wdk_sys::STATUS_DEVICE_NOT_READY;
        IoCompleteRequest(irp, IO_NO_INCREMENT);
        return wdk_sys::STATUS_DEVICE_NOT_READY;
    }
    let idx = (*ext).port_index as usize;
    let stack = IoGetCurrentIrpStackLocation(irp);
    let want = (*stack).Parameters.Read.Length as usize;
    let buf = (*irp).AssociatedIrp.SystemBuffer as *mut u8;
    if buf.is_null() {
        (*irp).IoStatus.Status = wdk_sys::STATUS_INVALID_PARAMETER;
        IoCompleteRequest(irp, IO_NO_INCREMENT);
        return wdk_sys::STATUS_INVALID_PARAMETER;
    }

    let irql = lock_pair(pc);
    let avail = (*pc).port[idx].rx.available_to_read();
    if avail == 0 {
        // Nothing to read: pend unless another read is already pending.
        if (*pc).port[idx].pending_read_irp != 0 {
            unlock_pair(pc, irql);
            (*irp).IoStatus.Information = 0;
            (*irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
            IoCompleteRequest(irp, IO_NO_INCREMENT);
            return wdk_sys::STATUS_SUCCESS;
        }
        (*pc).port[idx].pending_read_irp = irp as u64;
        unlock_pair(pc, irql);
        IoSetCancelRoutine(irp, Some(cancel_pending_irp));
        // Re-check cancellation race after arming the cancel routine.
        if (*irp).Cancel != 0 {
            IoSetCancelRoutine(irp, None);
            let irql2 = lock_pair(pc);
            if (*pc).port[idx].pending_read_irp == irp as u64 {
                (*pc).port[idx].pending_read_irp = 0;
                unlock_pair(pc, irql2);
                (*irp).IoStatus.Information = 0;
                (*irp).IoStatus.Status = wdk_sys::STATUS_CANCELLED;
                IoCompleteRequest(irp, IO_NO_INCREMENT);
                return wdk_sys::STATUS_CANCELLED;
            }
            unlock_pair(pc, irql2);
        }
        wdk_sys::STATUS_PENDING
    } else {
        let n = (*pc).port[idx]
            .rx
            .read(core::slice::from_raw_parts_mut(buf, want));
        (*pc).port[idx].stats.ReceivedCount =
            (*pc).port[idx].stats.ReceivedCount.wrapping_add(n as u32);
        unlock_pair(pc, irql);
        (*irp).IoStatus.Information = n as u64;
        (*irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(irp, IO_NO_INCREMENT);
        wdk_sys::STATUS_SUCCESS
    }
}

pub unsafe fn handle_write(ext: *mut PortExtension, irp: PIRP) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        (*irp).IoStatus.Status = wdk_sys::STATUS_DEVICE_NOT_READY;
        IoCompleteRequest(irp, IO_NO_INCREMENT);
        return wdk_sys::STATUS_DEVICE_NOT_READY;
    }
    let idx = (*ext).port_index as usize;
    let peer = 1 - idx;
    let stack = IoGetCurrentIrpStackLocation(irp);
    let len = (*stack).Parameters.Write.Length as usize;
    let buf = (*irp).AssociatedIrp.SystemBuffer as *mut u8;
    if buf.is_null() {
        (*irp).IoStatus.Status = wdk_sys::STATUS_INVALID_PARAMETER;
        IoCompleteRequest(irp, IO_NO_INCREMENT);
        return wdk_sys::STATUS_INVALID_PARAMETER;
    }

    let irql = lock_pair(pc);
    let written = (*pc).port[peer]
        .rx
        .write(core::slice::from_raw_parts(buf, len));
    (*pc).port[idx].stats.TransmittedCount = (*pc).port[idx]
        .stats
        .TransmittedCount
        .wrapping_add(written as u32);

    // Satisfy a peer pended read if present.
    let mut peer_irp: *mut wdk_sys::IRP = ptr::null_mut();
    let mut peer_n: usize = 0;
    if (*pc).port[peer].pending_read_irp != 0 {
        let pirp = (*pc).port[peer].pending_read_irp as *mut wdk_sys::IRP;
        let pstack = IoGetCurrentIrpStackLocation(pirp);
        let pwant = (*pstack).Parameters.Read.Length as usize;
        let pbuf = (*pirp).AssociatedIrp.SystemBuffer as *mut u8;
        peer_n = (*pc).port[peer]
            .rx
            .read(core::slice::from_raw_parts_mut(pbuf, pwant));
        (*pc).port[peer].pending_read_irp = 0;
        peer_irp = pirp;
    }
    // Flag a RXCHAR event for the peer.
    (*pc).port[peer].pending_events |= SERIAL_EV_RXCHAR;

    // Satisfy a peer pended WAIT_ON_MASK if its mask wants RXCHAR.
    let mut peer_event_irp: *mut wdk_sys::IRP = ptr::null_mut();
    let mut peer_event_mask: u32 = 0;
    if (*pc).port[peer].pending_event_irp != 0
        && ((*pc).port[peer].wait_mask & SERIAL_EV_RXCHAR) != 0
    {
        peer_event_irp = (*pc).port[peer].pending_event_irp as *mut wdk_sys::IRP;
        peer_event_mask = (*pc).port[peer].wait_mask & (*pc).port[peer].pending_events;
        (*pc).port[peer].pending_event_irp = 0;
        (*pc).port[peer].pending_events = 0;
    }
    unlock_pair(pc, irql);

    if !peer_irp.is_null() {
        (*peer_irp).IoStatus.Information = peer_n as u64;
        (*peer_irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(peer_irp, IO_NO_INCREMENT);
    }
    if !peer_event_irp.is_null() {
        // The output buffer of WAIT_ON_MASK is a ULONG mask of events.
        let ev_buf = (*peer_event_irp).AssociatedIrp.SystemBuffer as *mut u32;
        if !ev_buf.is_null() {
            *ev_buf = peer_event_mask;
        }
        (*peer_event_irp).IoStatus.Information = core::mem::size_of::<u32>() as u64;
        (*peer_event_irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(peer_event_irp, IO_NO_INCREMENT);
    }

    (*irp).IoStatus.Information = written as u64;
    (*irp).IoStatus.Status = wdk_sys::STATUS_SUCCESS;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    wdk_sys::STATUS_SUCCESS
}

/// Complete any pended read/event IRP for this port (used on cleanup/remove).
pub unsafe fn cancel_all(ext: *mut PortExtension) {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        return;
    }
    let idx = (*ext).port_index as usize;
    let irql = lock_pair(pc);
    let read_irp = (*pc).port[idx].pending_read_irp as *mut wdk_sys::IRP;
    let event_irp = (*pc).port[idx].pending_event_irp as *mut wdk_sys::IRP;
    (*pc).port[idx].pending_read_irp = 0;
    (*pc).port[idx].pending_event_irp = 0;
    unlock_pair(pc, irql);
    if !read_irp.is_null() {
        (*read_irp).IoStatus.Information = 0;
        (*read_irp).IoStatus.Status = wdk_sys::STATUS_CANCELLED;
        IoCompleteRequest(read_irp, IO_NO_INCREMENT);
    }
    if !event_irp.is_null() {
        (*event_irp).IoStatus.Information = 0;
        (*event_irp).IoStatus.Status = wdk_sys::STATUS_CANCELLED;
        IoCompleteRequest(event_irp, IO_NO_INCREMENT);
    }
}

/// Default cancel routine for pended read / WAIT_ON_MASK IRPs.
unsafe extern "C" fn cancel_pending_irp(device: PDEVICE_OBJECT, irp: PIRP) {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    let pc = (*ext).pair_va as *mut PairContext;
    let idx = (*ext).port_index as usize;
    // Release the cancel spinlock the I/O manager holds before doing work.
    wdk_sys::IoReleaseCancelSpinLock((*irp).CancelIrql);

    let irql = lock_pair(pc);
    if (*pc).port[idx].pending_read_irp == irp as u64 {
        (*pc).port[idx].pending_read_irp = 0;
        unlock_pair(pc, irql);
    } else if (*pc).port[idx].pending_event_irp == irp as u64 {
        (*pc).port[idx].pending_event_irp = 0;
        unlock_pair(pc, irql);
    } else {
        unlock_pair(pc, irql);
    }
    (*irp).IoStatus.Information = 0;
    (*irp).IoStatus.Status = wdk_sys::STATUS_CANCELLED;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
}

use core::ptr;
