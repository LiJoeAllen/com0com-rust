//! Cross-port data path + cancel-safe pending read/event IRPs.
//!
//! A write to port X copies into the *peer* port's RX ring buffer. If the peer
//! has a pended read (or a pended WAIT_ON_MASK), it is satisfied immediately.
//! All shared-state access is guarded by the `PairContext` spinlock.
//!
//! Emulation (baud pacing, noise, overrun, XOFF) is applied here, but the
//! decision logic itself lives in `com0com_common::emulation` so that it can be
//! unit tested without the WDK.

use core::ptr;

use wdk_sys::{
    ntddk::{
        IoAcquireCancelSpinLock, IoReleaseCancelSpinLock, IofCompleteRequest as IoCompleteRequest,
        KeAcquireSpinLockRaiseToDpc, KeQueryInterruptTimePrecise, KeReleaseSpinLock,
    },
    IO_NO_INCREMENT, KIRQL, KSPIN_LOCK, NTSTATUS, PDEVICE_OBJECT, PIRP,
};

use com0com_common::{
    constants::{SERIAL_EV_ERR, SERIAL_EV_RXCHAR},
    emulation::{frame_half_bits, inject_noise, LinePacer, XoffState, Xorshift32},
    pair::PairContext,
};

use crate::driver::{
    current_irp_stack_location, mark_irp_pending, set_cancel_routine, PortExtension,
};

/// Scratch buffer used to corrupt data in place when `emu_noise` is enabled.
/// Deliberately small: it lives on the (very limited) kernel stack, so large
/// writes are processed in `NOISE_CHUNK`-sized pieces.
const NOISE_CHUNK: usize = 256;

unsafe fn lock_pair(pc: *mut PairContext) -> KIRQL {
    KeAcquireSpinLockRaiseToDpc(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK)
}

#[inline]
unsafe fn unlock_pair(pc: *mut PairContext, irql: KIRQL) {
    KeReleaseSpinLock(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK, irql);
}

/// **IRP ownership, in one sentence: whoever successfully removes the cancel
/// routine owns the IRP; the `pending_*_irp` slot is only where the data
/// hangs.**
///
/// `IoCancelIrp` takes the cancel routine (under the cancel spinlock) *before*
/// dispatching it, so a later `set_cancel_routine(irp, None)` observes `None`
/// exactly when cancellation already won. That makes the return value the
/// single atomic ownership arbiter:
///
/// * `Some(routine)` — nobody had cancelled, we removed a still-installed
///   routine, so we are the only completer.
/// * `None` — either no routine was ever installed (an IRP that was never
///   published) or `cancel_pending_irp` already took it; in both cases someone
///   else owns the IRP and we must keep our hands off.
///
/// Every path that clears a `pending_*_irp` slot must therefore complete the
/// IRP *only* if this returns `true`.
///
/// Must be called after releasing the pair spinlock. When both locks are
/// needed the order is fixed at pair -> cancel (the publish paths take the
/// cancel spinlock while holding the pair lock); the cancel routine itself
/// releases the cancel spinlock before taking the pair lock, so the order is
/// never inverted.
///
/// The generated WDK binding defines `PDRIVER_CANCEL` as
/// `Option<unsafe extern "C" fn(...)>`, so `.is_some()` is the correct test.
///
/// # Safety
/// `irp` must be a live IRP for this device, and the pair spinlock must not be
/// held (the two locks are never held at the same time).
pub unsafe fn claim_irp(irp: PIRP) -> bool {
    let mut cancel_irql: KIRQL = 0;
    // Generated in wdk_sys::ntddk.
    IoAcquireCancelSpinLock(&mut cancel_irql);
    let prev = set_cancel_routine(irp, None);
    // Paired with IoAcquireCancelSpinLock above.
    IoReleaseCancelSpinLock(cancel_irql);
    prev.is_some()
}

/// IRPs collected under the pair lock that must be completed after unlocking.
pub struct PeerWake {
    pub read_irp: PIRP,
    pub read_len: usize,
    pub event_irp: PIRP,
    pub event_mask: u32,
}

impl PeerWake {
    const fn none() -> Self {
        PeerWake {
            read_irp: ptr::null_mut(),
            read_len: 0,
            event_irp: ptr::null_mut(),
            event_mask: 0,
        }
    }
}

/// Collect whatever peer IRPs a delivery of `delivered` bytes can satisfy.
///
/// Caller must hold the pair lock. Slots are cleared here (so no other path can
/// pick the same IRP up), and the IRPs themselves are completed later by
/// [`finish_peer_wake`] — completing while holding the pair lock would let the
/// I/O manager re-enter our dispatch routines with the lock held.
///
/// # Safety
/// `pc` must be a live `PairContext` whose spinlock the caller already holds.
pub unsafe fn wake_peer_locked(pc: *mut PairContext, idx: usize, delivered: usize) -> PeerWake {
    let peer = 1 - idx;
    if delivered == 0 {
        // Nothing arrived: never wake a reader with 0 bytes, and never claim a
        // RXCHAR that did not happen.
        return PeerWake::none();
    }
    let mut wake = PeerWake::none();

    // Flag a RXCHAR event for the peer.
    (*pc).port[peer].pending_events |= SERIAL_EV_RXCHAR;

    // Satisfy a peer pended read if present.
    if (*pc).port[peer].pending_read_irp != 0 {
        let pirp = (*pc).port[peer].pending_read_irp as *mut wdk_sys::IRP;
        let pstack = current_irp_stack_location(pirp);
        let pwant = (*pstack).Parameters.Read.Length as usize;
        let pbuf = (*pirp).AssociatedIrp.SystemBuffer as *mut u8;
        wake.read_len = (*pc).port[peer]
            .rx
            .read(core::slice::from_raw_parts_mut(pbuf, pwant));
        (*pc).port[peer].pending_read_irp = 0;
        wake.read_irp = pirp;
    }

    // Satisfy a peer pended WAIT_ON_MASK if its mask wants RXCHAR.
    if (*pc).port[peer].pending_event_irp != 0
        && ((*pc).port[peer].wait_mask & SERIAL_EV_RXCHAR) != 0
    {
        wake.event_irp = (*pc).port[peer].pending_event_irp as *mut wdk_sys::IRP;
        wake.event_mask = (*pc).port[peer].wait_mask & (*pc).port[peer].pending_events;
        (*pc).port[peer].pending_event_irp = 0;
        (*pc).port[peer].pending_events = 0;
    }
    wake
}

/// Complete the IRPs collected by [`wake_peer_locked`]. Must be called after
/// releasing the pair lock.
///
/// # Safety
/// `wake` must come from [`wake_peer_locked`] on this thread and must not have
/// been finished already.
pub unsafe fn finish_peer_wake(wake: &PeerWake) {
    if !wake.read_irp.is_null() && claim_irp(wake.read_irp) {
        (*wake.read_irp).IoStatus.Information = wake.read_len as u64;
        (*wake.read_irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(wake.read_irp, IO_NO_INCREMENT as i8);
    }
    // else: `cancel_pending_irp` got there first and owns it — leave it alone.
    if !wake.event_irp.is_null() && claim_irp(wake.event_irp) {
        // The output buffer of WAIT_ON_MASK is a ULONG mask of events.
        let ev_buf = (*wake.event_irp).AssociatedIrp.SystemBuffer as *mut u32;
        if !ev_buf.is_null() {
            *ev_buf = wake.event_mask;
        }
        (*wake.event_irp).IoStatus.Information = core::mem::size_of::<u32>() as u64;
        (*wake.event_irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(wake.event_irp, IO_NO_INCREMENT as i8);
    }
}

/// `IRP_MJ_READ`.
///
/// Reads are deliberately *not* rate limited: `emu_br` paces the writer, and
/// throttling the reader as well would need a second timer/DPC (the peer's RX
/// ring already bounds how much can be delivered). Data is only copied out when
/// it is already queued.
///
/// # Safety
/// `ext` and `irp` must be the live extension and current IRP of an
/// `IRP_MJ_READ` dispatch for this device.
pub unsafe fn handle_read(ext: *mut PortExtension, irp: PIRP) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        (*irp).IoStatus.Information = 0;
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_DEVICE_NOT_READY;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        return wdk_sys::STATUS_DEVICE_NOT_READY;
    }
    let idx = (*ext).port_index as usize;
    let stack = current_irp_stack_location(irp);
    let want = (*stack).Parameters.Read.Length as usize;
    let buf = (*irp).AssociatedIrp.SystemBuffer as *mut u8;
    if buf.is_null() {
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_INVALID_PARAMETER;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        return wdk_sys::STATUS_INVALID_PARAMETER;
    }

    // Fast paths complete synchronously and never install a cancel routine:
    // `IoCancelIrp` on an IRP without one only sets `Cancel` and leaves the
    // completion to us, so the direct `IoCompleteRequest` below is safe.
    //
    // The pending path publishes under BOTH locks (pair -> cancel, a fixed
    // order nobody violates in reverse): the cancel lock keeps `IoCancelIrp`
    // out while we mark pending, install the routine and store the IRP in the
    // slot, so by the time a cancel can be dispatched the IRP is guaranteed to
    // be in the slot. The old arm-first order left a window where the cancel
    // routine completed (and the I/O manager recycled) an IRP this function
    // was still about to publish.
    let irql = lock_pair(pc);
    let avail = (*pc).port[idx].rx.available_to_read();
    if avail > 0 {
        let n = (*pc).port[idx]
            .rx
            .read(core::slice::from_raw_parts_mut(buf, want));
        (*pc).port[idx].stats.ReceivedCount =
            (*pc).port[idx].stats.ReceivedCount.wrapping_add(n as u32);
        unlock_pair(pc, irql);
        (*irp).IoStatus.Information = n as u64;
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        wdk_sys::STATUS_SUCCESS
    } else if (*pc).port[idx].pending_read_irp != 0 {
        // Another read is already pended: complete this one empty.
        unlock_pair(pc, irql);
        (*irp).IoStatus.Information = 0;
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        wdk_sys::STATUS_SUCCESS
    } else {
        // Publish. Holding the cancel spinlock makes "check Cancel, install the
        // routine, mark pending, store the slot" atomic with respect to
        // `IoCancelIrp`; holding the pair lock makes it atomic with respect to
        // the peer writer, which is what lets us trust `avail == 0` above.
        // Both cancel-spinlock functions are generated in wdk_sys::ntddk.
        let mut cancel_irql: KIRQL = 0;
        IoAcquireCancelSpinLock(&mut cancel_irql);
        if (*irp).Cancel != 0 {
            // Cancelled before we armed anything: we are the only owner (no
            // routine is installed), so complete it ourselves.
            IoReleaseCancelSpinLock(cancel_irql);
            unlock_pair(pc, irql);
            (*irp).IoStatus.Information = 0;
            (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_CANCELLED;
            IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
            return wdk_sys::STATUS_CANCELLED;
        }
        set_cancel_routine(irp, Some(cancel_pending_irp));
        // Mark pending before publishing so a peer that picks the IRP up at
        // once cannot hand the I/O manager a non-pending IRP to recycle.
        // The WDK inline marks the current stack location's Control field.
        mark_irp_pending(irp);
        (*pc).port[idx].pending_read_irp = irp as u64;
        IoReleaseCancelSpinLock(cancel_irql);
        unlock_pair(pc, irql);
        // The IRP is owned by the slot now; do not touch it again.
        wdk_sys::STATUS_PENDING
    }
}

/// `IRP_MJ_WRITE`, with the P2 emulation features wired in.
///
/// **Short-write trade-off (important):** when `emu_br` is enabled the pacer
/// hands out a byte budget per call. Whatever does not fit in the budget is
/// *not* staged in a shadow buffer and no timer/DPC is created to drain it —
/// the write simply completes with `Information = <bytes actually delivered>`.
/// A short write is legal for a serial port (`WriteFile` returns the byte
/// count), callers retry, and the pacer refills with elapsed time, so the
/// effective throughput is still the configured baud rate. Staging would need
/// an extra non-paged buffer plus a DPC per pair, which is a lot of complexity
/// (and a lot of untestable kernel code) for the same observable behaviour.
///
/// # Safety
/// `ext` and `irp` must be the live extension and current IRP of an
/// `IRP_MJ_WRITE` dispatch for this device.
pub unsafe fn handle_write(ext: *mut PortExtension, irp: PIRP) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_DEVICE_NOT_READY;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        return wdk_sys::STATUS_DEVICE_NOT_READY;
    }
    let idx = (*ext).port_index as usize;
    let peer = 1 - idx;
    let stack = current_irp_stack_location(irp);
    let len = (*stack).Parameters.Write.Length as usize;
    let buf = (*irp).AssociatedIrp.SystemBuffer as *mut u8;
    if buf.is_null() {
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_INVALID_PARAMETER;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        return wdk_sys::STATUS_INVALID_PARAMETER;
    }

    let irql = lock_pair(pc);
    // Emulation parameters are stored in the shared PairContext: the port
    // driver has no other way to read `emu_*` (they live in the bus slot).
    let params = (*pc).params;

    // `plugin_mode` / `exclusive_mode` are pure visibility policies handled by
    // the bus during enumeration; they do not alter the data path, so they are
    // intentionally not consulted here.

    // --- 1) How many bytes may this write deliver? -------------------------
    let mut allowed: usize = len;

    // 1a) XOFF counter (`IOCTL_SERIAL_XOFF_COUNTER` / `IOCTL_SERIAL_SET_XOFF`).
    let mut xoff = XoffState {
        remaining: (*pc).port[idx].xoff_remaining,
        halted: (*pc).port[idx].tx_halted != 0,
    };
    allowed = xoff.admit(allowed);
    (*pc).port[idx].xoff_remaining = xoff.remaining;
    (*pc).port[idx].tx_halted = if xoff.halted { 1 } else { 0 };

    // 1b) Baud-rate emulation (`emu_br`): cap by what the line should have
    //     transmitted since the previous write. `u32::MAX` means "unlimited".
    if params.emu_br != 0 && allowed != 0 {
        // KeQueryInterruptTime is a header macro; the generated callable
        // equivalent is KeQueryInterruptTimePrecise, also returning 100ns units.
        // Its second argument is `_Out_ PULONG64` — the kernel writes the
        // timestamp through it, so NULL is not an option: that was the 0xA
        // write-to-0 bugcheck at DISPATCH_LEVEL in v8.
        let mut stamp: u64 = 0;
        let now = KeQueryInterruptTimePrecise(&mut stamp);
        let lc = (*pc).port[idx].line_control; // [StopBits, Parity, WordLength]
        let half_bits = frame_half_bits(lc[2], lc[1], lc[0]);
        let mut pacer = LinePacer {
            last_100ns: (*pc).port[idx].pacer_last_100ns,
            carry_half_bits: (*pc).port[idx].pacer_carry_half_bits,
        };
        let budget = pacer.bytes_available(now, (*pc).port[idx].baud, half_bits);
        (*pc).port[idx].pacer_last_100ns = pacer.last_100ns;
        (*pc).port[idx].pacer_carry_half_bits = pacer.carry_half_bits;
        allowed = allowed.min(budget as usize);
    }

    if allowed == 0 {
        // Nothing may be delivered right now (XOFF halt, or the pacer has no
        // budget yet). Complete with 0 bytes; the caller retries. No overrun is
        // charged for this — the data was never handed to the line.
        unlock_pair(pc, irql);
        (*irp).IoStatus.Information = 0;
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        return wdk_sys::STATUS_SUCCESS;
    }

    // --- 2) Deliver into the peer's RX ring, honouring its queue limit. ----
    let cap = (*pc).port[peer].queue_limit as usize;
    let written: usize;
    if params.emu_noise != 0 {
        // Noise path: stage in small chunks, corrupt in place, then queue. The
        // default (no-noise) path below stays zero-copy.
        let mut rng = Xorshift32::new((*pc).port[idx].rng_state);
        let mut chunk = [0u8; NOISE_CHUNK];
        let mut corrupted: usize = 0;
        written = {
            let mut total: usize = 0;
            while total < allowed {
                let chunk_len = (allowed - total).min(NOISE_CHUNK);
                ptr::copy_nonoverlapping(buf.add(total), chunk.as_mut_ptr(), chunk_len);
                corrupted +=
                    inject_noise(&mut rng, params.noise_probability, &mut chunk[..chunk_len]);
                let n = (*pc).port[peer].rx.write_capped(&chunk[..chunk_len], cap);
                total += n;
                if n < chunk_len {
                    // The peer's queue is at its cap; the rest is dropped.
                    break;
                }
            }
            total
        };
        (*pc).port[idx].rng_state = rng.0;
        if corrupted != 0 && written != 0 {
            // Only bytes that reached the ring can look corrupt to the reader,
            // so never report more corruption than we delivered.
            let seen = corrupted.min(written) as u32;
            (*pc).port[peer].noise_count = (*pc).port[peer].noise_count.wrapping_add(seen);
            // Simulated corruption is surfaced once per write as both a frame
            // and a parity error (a real UART would pick one; both counters
            // moving is the observable "noise happened" signal).
            (*pc).port[peer].stats.FrameErrorCount =
                (*pc).port[peer].stats.FrameErrorCount.wrapping_add(1);
            (*pc).port[peer].stats.ParityErrorCount =
                (*pc).port[peer].stats.ParityErrorCount.wrapping_add(1);
        }
    } else {
        written = (*pc).port[peer]
            .rx
            .write_capped(core::slice::from_raw_parts(buf, allowed), cap);
    }

    // --- 3) Overrun emulation (`emu_overrun`) ------------------------------
    // Without the flag, truncation at the queue limit is ordinary flow control
    // and must not be reported as an error.
    if params.emu_overrun != 0 && written < allowed {
        let lost = (allowed - written) as u32;
        (*pc).port[peer].stats.SerialOverrunErrorCount = (*pc).port[peer]
            .stats
            .SerialOverrunErrorCount
            .wrapping_add(lost);
        (*pc).port[peer].stats.BufferOverrunErrorCount = (*pc).port[peer]
            .stats
            .BufferOverrunErrorCount
            .wrapping_add(lost);
        (*pc).port[peer].pending_events |= SERIAL_EV_ERR;
    }

    (*pc).port[idx].stats.TransmittedCount = (*pc).port[idx]
        .stats
        .TransmittedCount
        .wrapping_add(written as u32);

    // Wake whatever the peer has pended on this delivery (no-op when
    // `written == 0`, i.e. the peer's queue was full).
    let wake = wake_peer_locked(pc, idx, written);
    unlock_pair(pc, irql);
    finish_peer_wake(&wake);

    (*irp).IoStatus.Information = written as u64;
    (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_SUCCESS;
    IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
    wdk_sys::STATUS_SUCCESS
}

/// Cancel only the pended *read* IRP of this port (used by `PURGE_RXABORT`).
///
/// A pended `IOCTL_SERIAL_WAIT_ON_MASK` is a separate channel (`WaitCommEvent`)
/// and is deliberately left alone: `PurgeComm(PURGE_RXABORT)` ends outstanding
/// *reads*, it must not also kill an event wait the application relies on.
///
/// Same ownership protocol as [`cancel_all`]: the slot is cleared under the pair
/// lock and the IRP is completed afterwards, only if `claim_irp` wins it (a
/// racing `cancel_pending_irp` may own it instead).
///
/// # Safety
/// `ext` must be a live extension; the pair spinlock must not be held.
pub unsafe fn cancel_pending_read(ext: *mut PortExtension) {
    let pc = (*ext).pair_va as *mut PairContext;
    if pc.is_null() {
        return;
    }
    let idx = (*ext).port_index as usize;
    let irql = lock_pair(pc);
    let read_irp = (*pc).port[idx].pending_read_irp as *mut wdk_sys::IRP;
    (*pc).port[idx].pending_read_irp = 0;
    unlock_pair(pc, irql);
    if !read_irp.is_null() && claim_irp(read_irp) {
        (*read_irp).IoStatus.Information = 0;
        (*read_irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_CANCELLED;
        IoCompleteRequest(read_irp, IO_NO_INCREMENT as i8);
    }
}

/// Complete any pended read/event IRP for this port (used on cleanup/remove).
///
/// # Safety
/// `ext` must be a live extension; the pair spinlock must not be held.
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
    // Slots are cleared (data only); ownership is decided by `claim_irp`. If it
    // fails, `cancel_pending_irp` was dispatched and is completing the IRP.
    if !read_irp.is_null() && claim_irp(read_irp) {
        (*read_irp).IoStatus.Information = 0;
        (*read_irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_CANCELLED;
        IoCompleteRequest(read_irp, IO_NO_INCREMENT as i8);
    }
    if !event_irp.is_null() && claim_irp(event_irp) {
        (*event_irp).IoStatus.Information = 0;
        (*event_irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_CANCELLED;
        IoCompleteRequest(event_irp, IO_NO_INCREMENT as i8);
    }
}

/// Default cancel routine for pended read / WAIT_ON_MASK IRPs.
///
/// The I/O manager calls this with the cancel spinlock held; it is released
/// first (`(*irp).CancelIrql`) before the pair lock is taken. The lock order is
/// fixed at pair -> cancel (the publish paths hold the pair lock when acquiring
/// the cancel spinlock), so releasing it here keeps the order one-way.
///
/// **Ownership:** being dispatched means `IoCancelIrp` already took the cancel
/// routine, so we are the sole owner and must always complete — the slot is
/// only unlinked for tidiness, it is *not* the ownership token (see
/// [`claim_irp`]). Any racing path will see `IoSetCancelRoutine` return `None`
/// and leave the IRP to us.
///
/// # Safety
/// Invoked by the I/O manager with the cancel spinlock held; `device` and
/// `irp` must be the live device object and IRP being cancelled.
pub unsafe extern "C" fn cancel_pending_irp(device: PDEVICE_OBJECT, irp: PIRP) {
    let ext = (*device).DeviceExtension as *mut PortExtension;
    let pc = (*ext).pair_va as *mut PairContext;
    let idx = (*ext).port_index as usize;
    // Release the cancel spinlock the I/O manager holds before doing work.
    IoReleaseCancelSpinLock((*irp).CancelIrql);

    // 设备移除会先把 pair_va 清零再发 UNLINK；此时仍可能有挂起 IRP 被取消，
    // 不能再解引用 PairContext，直接按取消完成。
    if pc.is_null() {
        (*irp).IoStatus.Information = 0;
        (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_CANCELLED;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
        return;
    }

    // Unlink from whichever slot owns it so no other path picks it up. We do
    // *not* decide ownership from the slot: a racing writer may have cleared it
    // first, in which case that writer sees `IoSetCancelRoutine` return `None`
    // and leaves the IRP to us.
    let irql = lock_pair(pc);
    if (*pc).port[idx].pending_read_irp == irp as u64 {
        (*pc).port[idx].pending_read_irp = 0;
    } else if (*pc).port[idx].pending_event_irp == irp as u64 {
        (*pc).port[idx].pending_event_irp = 0;
    }
    unlock_pair(pc, irql);

    // We were dispatched, so we are the only completer.
    (*irp).IoStatus.Information = 0;
    (*irp).IoStatus.__bindgen_anon_1.Status = wdk_sys::STATUS_CANCELLED;
    IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
}
