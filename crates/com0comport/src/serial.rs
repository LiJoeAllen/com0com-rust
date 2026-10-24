//! `IOCTL_SERIAL_*` handling. Mirrors the inbox `serial.sys` surface so the
//! Win32 Comm API works transparently. All shared-state access is guarded by
//! the `PairContext` spinlock.

use core::{mem, ptr};

use wdk_sys::{
    ntddk::{
        IoAcquireCancelSpinLock, IoReleaseCancelSpinLock, IofCompleteRequest as IoCompleteRequest,
        KeAcquireSpinLockRaiseToDpc, KeReleaseSpinLock,
    },
    IO_NO_INCREMENT, KIRQL, KSPIN_LOCK, NTSTATUS, PIRP,
};

use com0com_common::{
    constants::{
        SERIAL_CTS_STATE, SERIAL_DSR_STATE, SERIAL_DTR_STATE, SERIAL_EV_RXCHAR, SERIAL_EV_RXFLAG,
        SERIAL_RTS_STATE,
    },
    emulation::{clamp_queue_size, XoffState},
    ioctls,
    pair::{PairContext, RING_CAPACITY},
    serial_structs::{
        SERIALPERF_STATS, SERIAL_BAUD_RATE, SERIAL_CHARS, SERIAL_COMMPROP, SERIAL_HANDFLOW,
        SERIAL_LINE_CONTROL, SERIAL_QUEUE_SIZE, SERIAL_STATUS, SERIAL_TIMEOUTS,
        SERIAL_XOFF_COUNTER,
    },
};

use crate::{
    driver::{current_irp_stack_location, mark_irp_pending, set_cancel_routine, PortExtension},
    readwrite,
};

// `PurgeComm` masks (winbase.h), as received by IOCTL_SERIAL_PURGE.
const PURGE_TXABORT: u32 = 0x0001;
const PURGE_RXABORT: u32 = 0x0002;
const PURGE_TXCLEAR: u32 = 0x0004;
const PURGE_RXCLEAR: u32 = 0x0008;
const PURGE_MASK_ALL: u32 = PURGE_TXABORT | PURGE_RXABORT | PURGE_TXCLEAR | PURGE_RXCLEAR;

#[inline]
unsafe fn lock(pc: *mut PairContext) -> KIRQL {
    KeAcquireSpinLockRaiseToDpc(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK)
}

#[inline]
unsafe fn unlock(pc: *mut PairContext, irql: KIRQL) {
    KeReleaseSpinLock(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK, irql);
}

unsafe fn read_in<T: Copy>(irp: PIRP, min_len: usize) -> Option<T> {
    let stack = current_irp_stack_location(irp);
    let in_len = (*stack).Parameters.DeviceIoControl.InputBufferLength as usize;
    if in_len < min_len || mem::size_of::<T>() > in_len {
        return None;
    }
    let buf = (*irp).AssociatedIrp.SystemBuffer as *mut u8;
    let mut v = mem::MaybeUninit::<T>::uninit();
    ptr::copy_nonoverlapping(buf, v.as_mut_ptr() as *mut u8, mem::size_of::<T>());
    Some(v.assume_init())
}

unsafe fn write_out<T: Copy>(irp: PIRP, val: &T) -> usize {
    let stack = current_irp_stack_location(irp);
    let out_len = (*stack).Parameters.DeviceIoControl.OutputBufferLength as usize;
    let n = mem::size_of::<T>().min(out_len);
    let buf = (*irp).AssociatedIrp.SystemBuffer as *mut u8;
    ptr::copy_nonoverlapping(val as *const T as *const u8, buf, n);
    n
}

#[inline]
fn complete(irp: PIRP, status: NTSTATUS, info: usize) -> NTSTATUS {
    unsafe {
        (*irp).IoStatus.Information = info as u64;
        (*irp).IoStatus.__bindgen_anon_1.Status = status;
        IoCompleteRequest(irp, IO_NO_INCREMENT as i8);
    }
    status
}

/// Dispatch the serial `IOCTL_SERIAL_*` surface.
///
/// # Safety
/// `ext` and `irp` must be the live extension and current IRP of an
/// `IRP_MJ_DEVICE_CONTROL` dispatch for this device.
pub unsafe fn dispatch_serial(ext: *mut PortExtension, irp: PIRP) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    let stack = current_irp_stack_location(irp);
    let ioctl = (*stack).Parameters.DeviceIoControl.IoControlCode;
    let idx = (*ext).port_index as usize;

    if pc.is_null() {
        return complete(irp, wdk_sys::STATUS_DEVICE_NOT_READY, 0);
    }

    match ioctl {
        ioctls::IOCTL_SERIAL_GET_BAUD_RATE => {
            let irql = lock(pc);
            let v = SERIAL_BAUD_RATE {
                BaudRate: (*pc).port[idx].baud,
            };
            unlock(pc, irql);
            let n = write_out(irp, &v);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_BAUD_RATE => {
            let req: Option<SERIAL_BAUD_RATE> = read_in(irp, mem::size_of::<SERIAL_BAUD_RATE>());
            match req {
                Some(r) => {
                    let irql = lock(pc);
                    (*pc).port[idx].baud = r.BaudRate;
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_GET_LINE_CONTROL => {
            let irql = lock(pc);
            let lc = SERIAL_LINE_CONTROL {
                StopBits: (*pc).port[idx].line_control[0],
                Parity: (*pc).port[idx].line_control[1],
                WordLength: (*pc).port[idx].line_control[2],
            };
            unlock(pc, irql);
            let n = write_out(irp, &lc);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_LINE_CONTROL => {
            let req: Option<SERIAL_LINE_CONTROL> =
                read_in(irp, mem::size_of::<SERIAL_LINE_CONTROL>());
            match req {
                Some(lc) => {
                    let irql = lock(pc);
                    (*pc).port[idx].line_control = [lc.StopBits, lc.Parity, lc.WordLength];
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_GET_CHARS => {
            let irql = lock(pc);
            let c = SERIAL_CHARS {
                EofChar: (*pc).port[idx].chars[0],
                ErrorChar: (*pc).port[idx].chars[1],
                BreakChar: (*pc).port[idx].chars[2],
                EventChar: (*pc).port[idx].chars[3],
                XonChar: (*pc).port[idx].chars[4],
                XoffChar: (*pc).port[idx].chars[5],
            };
            unlock(pc, irql);
            let n = write_out(irp, &c);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_CHARS => {
            let req: Option<SERIAL_CHARS> = read_in(irp, mem::size_of::<SERIAL_CHARS>());
            match req {
                Some(c) => {
                    let irql = lock(pc);
                    (*pc).port[idx].chars = [
                        c.EofChar,
                        c.ErrorChar,
                        c.BreakChar,
                        c.EventChar,
                        c.XonChar,
                        c.XoffChar,
                    ];
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_GET_TIMEOUTS => {
            let irql = lock(pc);
            let t = SERIAL_TIMEOUTS {
                ReadIntervalTimeout: (*pc).port[idx].timeouts[0],
                ReadTotalTimeoutMultiplier: (*pc).port[idx].timeouts[1],
                ReadTotalTimeoutConstant: (*pc).port[idx].timeouts[2],
                WriteTotalTimeoutMultiplier: (*pc).port[idx].timeouts[3],
                WriteTotalTimeoutConstant: (*pc).port[idx].timeouts[4],
            };
            unlock(pc, irql);
            let n = write_out(irp, &t);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_TIMEOUTS => {
            let req: Option<SERIAL_TIMEOUTS> = read_in(irp, mem::size_of::<SERIAL_TIMEOUTS>());
            match req {
                Some(t) => {
                    let irql = lock(pc);
                    (*pc).port[idx].timeouts = [
                        t.ReadIntervalTimeout,
                        t.ReadTotalTimeoutMultiplier,
                        t.ReadTotalTimeoutConstant,
                        t.WriteTotalTimeoutMultiplier,
                        t.WriteTotalTimeoutConstant,
                    ];
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_GET_HANDFLOW => {
            let irql = lock(pc);
            let h = SERIAL_HANDFLOW {
                ControlHandShake: (*pc).port[idx].handflow[0],
                FlowReplace: (*pc).port[idx].handflow[1],
                XonLimit: (*pc).port[idx].handflow[2] as i32,
                XoffLimit: (*pc).port[idx].handflow[3] as i32,
            };
            unlock(pc, irql);
            let n = write_out(irp, &h);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_HANDFLOW => {
            let req: Option<SERIAL_HANDFLOW> = read_in(irp, mem::size_of::<SERIAL_HANDFLOW>());
            match req {
                Some(h) => {
                    let irql = lock(pc);
                    (*pc).port[idx].handflow = [
                        h.ControlHandShake,
                        h.FlowReplace,
                        h.XonLimit as u32,
                        h.XoffLimit as u32,
                    ];
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_GET_MODEMSTATUS => {
            let irql = lock(pc);
            let peer = 1 - idx;
            let peer_mcr = (*pc).port[peer].modem_control;
            let mut status: u32 =
                (*pc).port[idx].modem_control & (SERIAL_DTR_STATE | SERIAL_RTS_STATE) as u32;
            if (peer_mcr & SERIAL_DTR_STATE as u32) != 0 {
                status |= SERIAL_DSR_STATE as u32;
            }
            if (peer_mcr & SERIAL_RTS_STATE as u32) != 0 {
                status |= SERIAL_CTS_STATE as u32;
            }
            unlock(pc, irql);
            let out: u32 = status;
            let n = write_out(irp, &out);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_MODEM_CONTROL
        | ioctls::IOCTL_SERIAL_SET_DTR
        | ioctls::IOCTL_SERIAL_CLR_DTR
        | ioctls::IOCTL_SERIAL_SET_RTS
        | ioctls::IOCTL_SERIAL_CLR_RTS => {
            let irql = lock(pc);
            match ioctl {
                ioctls::IOCTL_SERIAL_SET_DTR => {
                    (*pc).port[idx].modem_control |= SERIAL_DTR_STATE as u32
                }
                ioctls::IOCTL_SERIAL_CLR_DTR => {
                    (*pc).port[idx].modem_control &= !(SERIAL_DTR_STATE as u32)
                }
                ioctls::IOCTL_SERIAL_SET_RTS => {
                    (*pc).port[idx].modem_control |= SERIAL_RTS_STATE as u32
                }
                ioctls::IOCTL_SERIAL_CLR_RTS => {
                    (*pc).port[idx].modem_control &= !(SERIAL_RTS_STATE as u32)
                }
                ioctls::IOCTL_SERIAL_SET_MODEM_CONTROL => {
                    // Takes a ULONG; set DTR/RTS bits accordingly.
                    let req: Option<u32> = read_in(irp, 4);
                    if let Some(m) = req {
                        if (m & SERIAL_DTR_STATE as u32) != 0 {
                            (*pc).port[idx].modem_control |= SERIAL_DTR_STATE as u32;
                        }
                        if (m & SERIAL_RTS_STATE as u32) != 0 {
                            (*pc).port[idx].modem_control |= SERIAL_RTS_STATE as u32;
                        }
                    }
                }
                _ => {}
            }
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_GET_COMMSTATUS => {
            let irql = lock(pc);
            let s = SERIAL_STATUS {
                Errors: 0,
                HoldReasons: 0,
                AmountInInQueue: (*pc).port[idx].rx.available_to_read() as u32,
                AmountInOutQueue: 0,
                EofWait: 0,
                WaitForImmediate: 0,
            };
            unlock(pc, irql);
            let n = write_out(irp, &s);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_GET_PROPERTIES => {
            // The queues are the shared ring buffer; one slot is reserved to
            // tell "full" apart from "empty", so the usable maximum is
            // `RING_BUFFER_SIZE - 1`. `Current*Queue` reflects whatever
            // `IOCTL_SERIAL_SET_QUEUE_SIZE` last stored.
            let irql = lock(pc);
            // `RingBuffer::capacity()` — one slot is reserved to tell "full"
            // from "empty".
            let max_queue = RING_CAPACITY as u32;
            let cur_tx = (*pc).port[idx].tx_buffer_size;
            let cur_rx = (*pc).port[idx].rx_buffer_size;
            unlock(pc, irql);
            let p = SERIAL_COMMPROP {
                PacketLength: mem::size_of::<SERIAL_COMMPROP>() as u16,
                PacketVersion: 1,
                ServiceMask: com0com_common::constants::SERIAL_SP_RS232,
                Reserved1: 0,
                MaxTxQueue: max_queue,
                MaxRxQueue: max_queue,
                MaxBaud: com0com_common::constants::SERIAL_BAUD_USER,
                ProvSubType: com0com_common::constants::SERIAL_SP_RS232,
                ProvCapabilities: com0com_common::constants::COM0COM_PROV_CAPABILITIES,
                SettableParams: com0com_common::constants::COM0COM_SETTABLE_PARAMS,
                SettableBaud: com0com_common::constants::SERIAL_BAUD_USER,
                SettableData: com0com_common::constants::COM0COM_SETTABLE_DATA,
                SettableStopParity: com0com_common::constants::COM0COM_SETTABLE_STOP_PARITY,
                CurrentTxQueue: cur_tx,
                CurrentRxQueue: cur_rx,
                ProvSpec1: 0,
                ProvSpec2: 0,
                ProvChar: [0u16; 1],
            };
            let n = write_out(irp, &p);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_QUEUE_SIZE => {
            // The ring buffer itself keeps its physical size, but the *usable*
            // window is clamped to the requested InSize/OutSize. `queue_limit`
            // is what the write path uses as the cap, so a small request really
            // does produce overruns (and, with `emu_overrun`, error counts).
            let req: Option<SERIAL_QUEUE_SIZE> = read_in(irp, mem::size_of::<SERIAL_QUEUE_SIZE>());
            match req {
                Some(q) => {
                    // Same ceiling as `RingBuffer::capacity()` and as
                    // `PortState::new()`'s `queue_limit` default.
                    let max = RING_CAPACITY as u32;
                    let irql = lock(pc);
                    // 0 means "leave this queue alone" (per ntddser.h).
                    if q.InSize != 0 {
                        (*pc).port[idx].rx_buffer_size = clamp_queue_size(q.InSize, max);
                    }
                    if q.OutSize != 0 {
                        (*pc).port[idx].tx_buffer_size = clamp_queue_size(q.OutSize, max);
                    }
                    // Keep the limit inside the physically usable window even
                    // when InSize == 0 left `rx_buffer_size` at its default
                    // (RING_BUFFER_SIZE, one more than the ring can hold).
                    (*pc).port[idx].queue_limit = (*pc).port[idx].rx_buffer_size.min(max);
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_PURGE => {
            // winbase.h semantics:
            //   PURGE_RXCLEAR(0x8) -> discard *queued receive* data (this
            //     port's RX ring).
            //   PURGE_RXABORT(0x2) -> complete the pending read IRP as
            //     cancelled. Aborting I/O must NOT drop data that already
            //     arrived.
            //   PURGE_TXCLEAR(0x4) / PURGE_TXABORT(0x1) -> no-op: this driver
            //     has no TX staging buffer at all and never pends a write
            //     (see the short-write trade-off note on
            //     `readwrite::handle_write` — bytes go straight into the
            //     peer's RX ring), so there is nothing to clear or cancel.
            let req: Option<u32> = read_in(irp, 4);
            if let Some(mask) = req {
                if (mask & PURGE_MASK_ALL) != 0 {
                    let irql = lock(pc);
                    if (mask & PURGE_RXCLEAR) != 0 {
                        // 就地清空，避免 16 KiB 的栈上临时值。
                        (*pc).port[idx].rx.clear();
                        // Drop the "data waiting" event bits too, otherwise the
                        // next WAIT_ON_MASK would report RXCHAR for a buffer we
                        // just emptied.
                        (*pc).port[idx].pending_events &= !(SERIAL_EV_RXCHAR | SERIAL_EV_RXFLAG);
                    }
                    // Any purge also drops a pending XOFF counter: after
                    // clearing or aborting, the transmitter must run again.
                    if (mask & PURGE_MASK_ALL) != 0 {
                        (*pc).port[idx].xoff_remaining = 0;
                        (*pc).port[idx].tx_halted = 0;
                    }
                    unlock(pc, irql);
                }
                // Completing IRPs must happen outside the pair lock; reuse the
                // existing cleanup path instead of open-coding it here.
                // RXABORT ends outstanding *reads* only — a pended
                // WAIT_ON_MASK is a different channel and stays alive.
                // TXABORT/TXCLEAR are no-ops (no TX staging, no pended write).
                if (mask & PURGE_RXABORT) != 0 {
                    readwrite::cancel_pending_read(ext);
                }
            }
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_GET_WAIT_MASK => {
            let irql = lock(pc);
            let m = (*pc).port[idx].wait_mask;
            unlock(pc, irql);
            let out = m;
            let n = write_out(irp, &out);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_WAIT_MASK => {
            let req: Option<u32> = read_in(irp, 4);
            match req {
                Some(m) => {
                    let irql = lock(pc);
                    (*pc).port[idx].wait_mask = m;
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_WAIT_ON_MASK => {
            // Pend until an event in the mask occurs.
            //
            // Same protocol as `readwrite::handle_read`: the synchronous
            // branches never install a cancel routine (an `IoCancelIrp` without
            // one only sets `Cancel`, it never completes the IRP), and the
            // pending branch publishes the IRP under both locks (pair ->
            // cancel). Doing the slot store and the routine install under the
            // cancel spinlock closes the window where a cancel could complete
            // — and the I/O manager recycle — an IRP that was not published
            // yet.
            let irql = lock(pc);
            let mask = (*pc).port[idx].wait_mask;
            let pending = (*pc).port[idx].pending_events;
            if (mask & pending) != 0 {
                let out_mask = mask & pending;
                (*pc).port[idx].pending_events = 0;
                unlock(pc, irql);
                let out = out_mask;
                let n = write_out(irp, &out);
                return complete(irp, wdk_sys::STATUS_SUCCESS, n);
            }
            if (*pc).port[idx].pending_event_irp != 0 {
                unlock(pc, irql);
                return complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0);
            }
            // Both cancel-spinlock functions are generated in wdk_sys::ntddk.
            let mut cancel_irql: KIRQL = 0;
            IoAcquireCancelSpinLock(&mut cancel_irql);
            if (*irp).Cancel != 0 {
                IoReleaseCancelSpinLock(cancel_irql);
                unlock(pc, irql);
                return complete(irp, wdk_sys::STATUS_CANCELLED, 0);
            }
            set_cancel_routine(irp, Some(readwrite::cancel_pending_irp));
            // Mark pending first, then make it visible to peer/wait wakeups.
            // The WDK inline marks the current stack location's Control field.
            mark_irp_pending(irp);
            (*pc).port[idx].pending_event_irp = irp as u64;
            IoReleaseCancelSpinLock(cancel_irql);
            unlock(pc, irql);
            // Owned by the slot from here on; do not touch it again.
            wdk_sys::STATUS_PENDING
        }
        ioctls::IOCTL_SERIAL_GET_STATS => {
            let irql = lock(pc);
            let s = (*pc).port[idx].stats;
            unlock(pc, irql);
            let n = write_out(irp, &s);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_CLEAR_STATS => {
            let irql = lock(pc);
            (*pc).port[idx].stats = SERIALPERF_STATS {
                ReceivedCount: 0,
                TransmittedCount: 0,
                FrameErrorCount: 0,
                SerialOverrunErrorCount: 0,
                BufferOverrunErrorCount: 0,
                ParityErrorCount: 0,
            };
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_SET_BREAK_ON => {
            let irql = lock(pc);
            (*pc).port[idx].break_state = 1;
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_SET_BREAK_OFF => {
            let irql = lock(pc);
            (*pc).port[idx].break_state = 0;
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_RESET_DEVICE => {
            let irql = lock(pc);
            // 就地清空，避免 16 KiB 的栈上临时值。
            (*pc).port[idx].rx.clear();
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_GET_DTRRTS => {
            let irql = lock(pc);
            let out = (*pc).port[idx].modem_control & (SERIAL_DTR_STATE | SERIAL_RTS_STATE) as u32;
            unlock(pc, irql);
            let o = out;
            let n = write_out(irp, &o);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_XOFF_COUNTER => {
            // Arm the XOFF counter: `Counter` bytes may still be delivered to
            // the peer, after which the port halts until SET_XON / PURGE.
            // (`Timeout` and `Usable` are not emulated — there is no real line.)
            let req: Option<SERIAL_XOFF_COUNTER> =
                read_in(irp, mem::size_of::<SERIAL_XOFF_COUNTER>());
            match req {
                Some(x) => {
                    let count: u32 = if x.Counter > 0 { x.Counter as u32 } else { 0 };
                    let irql = lock(pc);
                    let mut st = XoffState {
                        remaining: (*pc).port[idx].xoff_remaining,
                        halted: (*pc).port[idx].tx_halted != 0,
                    };
                    st.on_set(count);
                    (*pc).port[idx].xoff_remaining = st.remaining;
                    (*pc).port[idx].tx_halted = if st.halted { 1 } else { 0 };
                    unlock(pc, irql);
                    complete(irp, wdk_sys::STATUS_SUCCESS, 0)
                }
                None => complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0),
            }
        }
        ioctls::IOCTL_SERIAL_SET_XOFF => {
            // Halt transmission immediately (no counter left to drain).
            let irql = lock(pc);
            (*pc).port[idx].xoff_remaining = 0;
            (*pc).port[idx].tx_halted = 1;
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_SET_XON => {
            // Resume transmission and drop any armed counter.
            let irql = lock(pc);
            let mut st = XoffState {
                remaining: (*pc).port[idx].xoff_remaining,
                halted: (*pc).port[idx].tx_halted != 0,
            };
            st.clear();
            (*pc).port[idx].xoff_remaining = st.remaining;
            (*pc).port[idx].tx_halted = 0;
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_IMMEDIATE_CHAR => {
            // Immediate char: a single-byte write to the peer that jumps ahead
            // of the queue. It goes through the same ring (honouring the peer's
            // queue limit) and must wake a pended read / WAIT_ON_MASK,
            // otherwise an "immediate" byte would only be observed after the
            // next ordinary write.
            let req: Option<u8> = read_in(irp, 1);
            if let Some(c) = req {
                let peer = 1 - idx;
                let irql = lock(pc);
                // Same cap as the normal write path, so an immediate char can
                // never exceed the peer's configured queue size.
                let cap = (*pc).port[peer].queue_limit as usize;
                let n = (*pc).port[peer].rx.write_capped(&[c], cap);
                // Reuse the write path's delivery tail: RXCHAR + pending read /
                // WAIT_ON_MASK wake-up, collected under the lock ...
                let wake = readwrite::wake_peer_locked(pc, idx, n);
                unlock(pc, irql);
                // ... and completed after releasing it.
                readwrite::finish_peer_wake(&wake);
            }
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        _ => complete(irp, wdk_sys::STATUS_INVALID_DEVICE_REQUEST, 0),
    }
}
