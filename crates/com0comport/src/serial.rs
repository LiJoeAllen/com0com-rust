//! `IOCTL_SERIAL_*` handling. Mirrors the inbox `serial.sys` surface so the
//! Win32 Comm API works transparently. All shared-state access is guarded by
//! the `PairContext` spinlock.

use core::{mem, ptr};

use wdk_sys::{
    IoCompleteRequest, IoGetCurrentIrpStackLocation, IoSetCancelRoutine, KeAcquireSpinLock,
    KeReleaseSpinLock, IO_NO_INCREMENT, KIRQL, KSPIN_LOCK, NTSTATUS, PIRP,
};

use com0com_common::{
    constants::{
        SERIAL_CTS_STATE, SERIAL_DCD_STATE, SERIAL_DSR_STATE, SERIAL_DTR_STATE, SERIAL_RI_STATE,
        SERIAL_RTS_STATE,
    },
    ioctls,
    pair::PairContext,
    serial_structs::{
        SERIALPERF_STATS, SERIAL_BAUD_RATE, SERIAL_CHARS, SERIAL_COMMPROP, SERIAL_HANDFLOW,
        SERIAL_LINE_CONTROL, SERIAL_QUEUE_SIZE, SERIAL_STATUS, SERIAL_TIMEOUTS,
        SERIAL_XOFF_COUNTER,
    },
};

use crate::{driver::PortExtension, readwrite};

#[inline]
unsafe fn lock(pc: *mut PairContext) -> KIRQL {
    let mut irql: KIRQL = 0;
    KeAcquireSpinLock(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK, &mut irql);
    irql
}

#[inline]
unsafe fn unlock(pc: *mut PairContext, irql: KIRQL) {
    KeReleaseSpinLock(&mut (*pc).lock as *mut u64 as *mut KSPIN_LOCK, irql);
}

unsafe fn read_in<T: Copy>(irp: PIRP, min_len: usize) -> Option<T> {
    let stack = IoGetCurrentIrpStackLocation(irp);
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
    let stack = IoGetCurrentIrpStackLocation(irp);
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
        (*irp).IoStatus.Status = status;
        IoCompleteRequest(irp, IO_NO_INCREMENT);
    }
    status
}

pub unsafe fn dispatch_serial(ext: *mut PortExtension, irp: PIRP) -> NTSTATUS {
    let pc = (*ext).pair_va as *mut PairContext;
    let stack = IoGetCurrentIrpStackLocation(irp);
    let ioctl = (*stack).Parameters.DeviceIoControl.IoControlCode;
    let idx = (*ext).port_index as usize;

    if pc.is_null() {
        return complete(irp, wdk_sys::STATUS_DEVICE_NOT_READY, 0);
    }

    macro_rules! guard {
        () => {
            lock(pc)
        };
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
            let mut out: u32 = status;
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
            let p = SERIAL_COMMPROP {
                PacketLength: mem::size_of::<SERIAL_COMMPROP>() as u16,
                PacketVersion: 1,
                ServiceMask: com0com_common::constants::SERIAL_SP_RS232,
                Reserved1: 0,
                MaxTxQueue: 0,
                MaxRxQueue: 0,
                MaxBaud: com0com_common::constants::SERIAL_BAUD_USER,
                ProvSubType: com0com_common::constants::SERIAL_SP_RS232,
                ProvCapabilities: com0com_common::constants::COM0COM_PROV_CAPABILITIES,
                SettableParams: com0com_common::constants::COM0COM_SETTABLE_PARAMS,
                SettableBaud: com0com_common::constants::SERIAL_BAUD_USER,
                SettableData: com0com_common::constants::COM0COM_SETTABLE_DATA,
                SettableStopParity: com0com_common::constants::COM0COM_SETTABLE_STOP_PARITY,
                CurrentTxQueue: (*pc).port[idx].tx_buffer_size,
                CurrentRxQueue: (*pc).port[idx].rx_buffer_size,
                ProvSpec1: 0,
                ProvSpec2: 0,
                ProvChar: [0u16; 1],
            };
            let n = write_out(irp, &p);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_SET_QUEUE_SIZE => {
            // Fixed-size buffers in this implementation; accept but no resize.
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_PURGE => {
            let req: Option<u32> = read_in(irp, 4);
            let irql = lock(pc);
            // PURGE_TXCLEAR / PURGE_RXCLEAR bit masks from winbase.h.
            if let Some(mask) = req {
                if (mask & 0x0001) != 0 || (mask & 0x0002) != 0 {
                    (*pc).port[idx].rx = com0com_common::pair::RingBuffer::new();
                }
            }
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_GET_WAIT_MASK => {
            let irql = lock(pc);
            let m = (*pc).port[idx].wait_mask;
            unlock(pc, irql);
            let mut out = m;
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
            let irql = lock(pc);
            let mask = (*pc).port[idx].wait_mask;
            let pending = (*pc).port[idx].pending_events;
            if (mask & pending) != 0 {
                let out_mask = mask & pending;
                (*pc).port[idx].pending_events = 0;
                unlock(pc, irql);
                let mut out = out_mask;
                let n = write_out(irp, &out);
                complete(irp, wdk_sys::STATUS_SUCCESS, n)
            } else if (*pc).port[idx].pending_event_irp != 0 {
                unlock(pc, irql);
                complete(irp, wdk_sys::STATUS_INVALID_PARAMETER, 0)
            } else {
                (*pc).port[idx].pending_event_irp = irp as u64;
                unlock(pc, irql);
                IoSetCancelRoutine(irp, Some(readwrite::cancel_pending_irp));
                if (*irp).Cancel != 0 {
                    IoSetCancelRoutine(irp, None);
                    let irql2 = lock(pc);
                    if (*pc).port[idx].pending_event_irp == irp as u64 {
                        (*pc).port[idx].pending_event_irp = 0;
                        unlock(pc, irql2);
                        complete(irp, wdk_sys::STATUS_CANCELLED, 0)
                    } else {
                        unlock(pc, irql2);
                    }
                }
                wdk_sys::STATUS_PENDING
            }
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
            (*pc).port[idx].rx = com0com_common::pair::RingBuffer::new();
            unlock(pc, irql);
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_GET_DTRRTS => {
            let irql = lock(pc);
            let out = (*pc).port[idx].modem_control & (SERIAL_DTR_STATE | SERIAL_RTS_STATE) as u32;
            unlock(pc, irql);
            let mut o = out;
            let n = write_out(irp, &o);
            complete(irp, wdk_sys::STATUS_SUCCESS, n)
        }
        ioctls::IOCTL_SERIAL_XOFF_COUNTER => {
            // XOFF counter emulation is a no-op in the MVP.
            let _req: Option<SERIAL_XOFF_COUNTER> =
                read_in(irp, mem::size_of::<SERIAL_XOFF_COUNTER>());
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_SET_XOFF | ioctls::IOCTL_SERIAL_SET_XON => {
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        ioctls::IOCTL_SERIAL_IMMEDIATE_CHAR => {
            // Immediate char: treat as a single-byte write to the peer.
            let req: Option<u8> = read_in(irp, 1);
            if let Some(c) = req {
                let peer = 1 - idx;
                let irql = lock(pc);
                let _ = (*pc).port[peer].rx.write(&[c]);
                (*pc).port[peer].pending_events |= com0com_common::constants::SERIAL_EV_RXCHAR;
                unlock(pc, irql);
            }
            complete(irp, wdk_sys::STATUS_SUCCESS, 0)
        }
        _ => complete(irp, wdk_sys::STATUS_INVALID_DEVICE_REQUEST, 0),
    }
}
