//! `SERIAL_*` data structures mirrored from the official Microsoft WDK
//! `ntddser.h`. Field order, types and sizes MUST match the WDK because the
//! Win32 Comm API and every serial application are compiled against those
//! layouts. Transcribed from WDK 10.0.16299; diff against your installed WDK
//! before shipping.

/// `SERIAL_BAUD_RATE`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_BAUD_RATE {
    pub BaudRate: u32,
}

/// `SERIAL_LINE_CONTROL`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_LINE_CONTROL {
    pub StopBits: u8,
    pub Parity: u8,
    pub WordLength: u8,
}

/// `SERIAL_CHARS`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_CHARS {
    pub EofChar: u8,
    pub ErrorChar: u8,
    pub BreakChar: u8,
    pub EventChar: u8,
    pub XonChar: u8,
    pub XoffChar: u8,
}

/// `SERIAL_TIMEOUTS`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_TIMEOUTS {
    pub ReadIntervalTimeout: u32,
    pub ReadTotalTimeoutMultiplier: u32,
    pub ReadTotalTimeoutConstant: u32,
    pub WriteTotalTimeoutMultiplier: u32,
    pub WriteTotalTimeoutConstant: u32,
}

/// `SERIAL_HANDFLOW`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_HANDFLOW {
    pub ControlHandShake: u32,
    pub FlowReplace: u32,
    pub XonLimit: i32,
    pub XoffLimit: i32,
}

/// `SERIAL_STATUS` (returned by `IOCTL_SERIAL_GET_COMMSTATUS`)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_STATUS {
    pub Errors: u32,
    pub HoldReasons: u32,
    pub AmountInInQueue: u32,
    pub AmountInOutQueue: u32,
    pub EofWait: u32,
    pub WaitForImmediate: u8,
}

/// `SERIAL_COMMPROP` (returned by `IOCTL_SERIAL_GET_PROPERTIES`)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_COMMPROP {
    pub PacketLength: u16,
    pub PacketVersion: u16,
    pub ServiceMask: u32,
    pub Reserved1: u32,
    pub MaxTxQueue: u32,
    pub MaxRxQueue: u32,
    pub MaxBaud: u32,
    pub ProvSubType: u32,
    pub ProvCapabilities: u32,
    pub SettableParams: u32,
    pub SettableBaud: u32,
    pub SettableData: u16,
    pub SettableStopParity: u16,
    pub CurrentTxQueue: u32,
    pub CurrentRxQueue: u32,
    pub ProvSpec1: u32,
    pub ProvSpec2: u32,
    pub ProvChar: [u16; 1],
}

/// `SERIAL_QUEUE_SIZE` (for `IOCTL_SERIAL_SET_QUEUE_SIZE`)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_QUEUE_SIZE {
    pub InSize: u32,
    pub OutSize: u32,
}

/// `SERIAL_XOFF_COUNTER` (official WDK layout: `LONG Usable`, not `UCHAR`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_XOFF_COUNTER {
    pub Timeout: u32,
    pub Counter: i32,
    pub Usable: i32,
}

/// `SERIALPERF_STATS` (returned by `IOCTL_SERIAL_GET_STATS`)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIALPERF_STATS {
    pub ReceivedCount: u32,
    pub TransmittedCount: u32,
    pub FrameErrorCount: u32,
    pub SerialOverrunErrorCount: u32,
    pub BufferOverrunErrorCount: u32,
    pub ParityErrorCount: u32,
}

/// `SERIAL_COMMCONFIG` is large and variable; we expose only its fixed prefix
/// here. Full support (GET/SET_COMMCONFIG) is a TODO in the port driver.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SERIAL_COMMCONFIG {
    pub Size: u32,
    pub Version: u16,
    pub Reserved: u16,
    pub BaudRate: u32,
    pub LineControl: SERIAL_LINE_CONTROL,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    // Sizes MUST match the WDK `ntddser.h` layouts byte-for-byte, otherwise the
    // Win32 Comm API (compiled against the WDK structs) and our driver disagree
    // on how many bytes each IOCTL transfers.
    #[test]
    fn struct_sizes_match_wdk() {
        assert_eq!(size_of::<SERIAL_BAUD_RATE>(), 4);
        assert_eq!(size_of::<SERIAL_LINE_CONTROL>(), 3);
        assert_eq!(size_of::<SERIAL_CHARS>(), 6);
        assert_eq!(size_of::<SERIAL_TIMEOUTS>(), 20);
        assert_eq!(size_of::<SERIAL_HANDFLOW>(), 16);
        assert_eq!(size_of::<SERIAL_STATUS>(), 24); // 21 payload + 3 pad to align 4
        assert_eq!(size_of::<SERIAL_COMMPROP>(), 64);
        assert_eq!(size_of::<SERIAL_QUEUE_SIZE>(), 8);
        assert_eq!(size_of::<SERIAL_XOFF_COUNTER>(), 12);
        assert_eq!(size_of::<SERIALPERF_STATS>(), 24);
        assert_eq!(size_of::<SERIAL_COMMCONFIG>(), 16); // fixed prefix only
    }

    #[test]
    fn field_offsets_match_wdk() {
        // SERIAL_BAUD_RATE
        assert_eq!(offset_of!(SERIAL_BAUD_RATE, BaudRate), 0);

        // SERIAL_LINE_CONTROL
        assert_eq!(offset_of!(SERIAL_LINE_CONTROL, StopBits), 0);
        assert_eq!(offset_of!(SERIAL_LINE_CONTROL, Parity), 1);
        assert_eq!(offset_of!(SERIAL_LINE_CONTROL, WordLength), 2);

        // SERIAL_CHARS
        assert_eq!(offset_of!(SERIAL_CHARS, EofChar), 0);
        assert_eq!(offset_of!(SERIAL_CHARS, ErrorChar), 1);
        assert_eq!(offset_of!(SERIAL_CHARS, BreakChar), 2);
        assert_eq!(offset_of!(SERIAL_CHARS, EventChar), 3);
        assert_eq!(offset_of!(SERIAL_CHARS, XonChar), 4);
        assert_eq!(offset_of!(SERIAL_CHARS, XoffChar), 5);

        // SERIAL_TIMEOUTS
        assert_eq!(offset_of!(SERIAL_TIMEOUTS, ReadIntervalTimeout), 0);
        assert_eq!(offset_of!(SERIAL_TIMEOUTS, ReadTotalTimeoutMultiplier), 4);
        assert_eq!(offset_of!(SERIAL_TIMEOUTS, ReadTotalTimeoutConstant), 8);
        assert_eq!(offset_of!(SERIAL_TIMEOUTS, WriteTotalTimeoutMultiplier), 12);
        assert_eq!(offset_of!(SERIAL_TIMEOUTS, WriteTotalTimeoutConstant), 16);

        // SERIAL_HANDFLOW
        assert_eq!(offset_of!(SERIAL_HANDFLOW, ControlHandShake), 0);
        assert_eq!(offset_of!(SERIAL_HANDFLOW, FlowReplace), 4);
        assert_eq!(offset_of!(SERIAL_HANDFLOW, XonLimit), 8);
        assert_eq!(offset_of!(SERIAL_HANDFLOW, XoffLimit), 12);

        // SERIAL_STATUS (21 payload, padded to 24)
        assert_eq!(offset_of!(SERIAL_STATUS, Errors), 0);
        assert_eq!(offset_of!(SERIAL_STATUS, HoldReasons), 4);
        assert_eq!(offset_of!(SERIAL_STATUS, AmountInInQueue), 8);
        assert_eq!(offset_of!(SERIAL_STATUS, AmountInOutQueue), 12);
        assert_eq!(offset_of!(SERIAL_STATUS, EofWait), 16);
        assert_eq!(offset_of!(SERIAL_STATUS, WaitForImmediate), 20);

        // SERIAL_COMMPROP
        assert_eq!(offset_of!(SERIAL_COMMPROP, PacketLength), 0);
        assert_eq!(offset_of!(SERIAL_COMMPROP, PacketVersion), 2);
        assert_eq!(offset_of!(SERIAL_COMMPROP, ServiceMask), 4);
        assert_eq!(offset_of!(SERIAL_COMMPROP, MaxTxQueue), 12);
        assert_eq!(offset_of!(SERIAL_COMMPROP, MaxBaud), 20);
        assert_eq!(offset_of!(SERIAL_COMMPROP, SettableData), 40);
        assert_eq!(offset_of!(SERIAL_COMMPROP, SettableStopParity), 42);
        assert_eq!(offset_of!(SERIAL_COMMPROP, CurrentTxQueue), 44);
        assert_eq!(offset_of!(SERIAL_COMMPROP, ProvChar), 60);

        // SERIAL_QUEUE_SIZE
        assert_eq!(offset_of!(SERIAL_QUEUE_SIZE, InSize), 0);
        assert_eq!(offset_of!(SERIAL_QUEUE_SIZE, OutSize), 4);

        // SERIAL_XOFF_COUNTER (official WDK layout: LONG Usable, not UCHAR)
        assert_eq!(offset_of!(SERIAL_XOFF_COUNTER, Timeout), 0);
        assert_eq!(offset_of!(SERIAL_XOFF_COUNTER, Counter), 4);
        assert_eq!(offset_of!(SERIAL_XOFF_COUNTER, Usable), 8);

        // SERIALPERF_STATS
        assert_eq!(offset_of!(SERIALPERF_STATS, ReceivedCount), 0);
        assert_eq!(offset_of!(SERIALPERF_STATS, TransmittedCount), 4);
        assert_eq!(offset_of!(SERIALPERF_STATS, FrameErrorCount), 8);
        assert_eq!(offset_of!(SERIALPERF_STATS, SerialOverrunErrorCount), 12);
        assert_eq!(offset_of!(SERIALPERF_STATS, BufferOverrunErrorCount), 16);
        assert_eq!(offset_of!(SERIALPERF_STATS, ParityErrorCount), 20);

        // SERIAL_COMMCONFIG (fixed prefix)
        assert_eq!(offset_of!(SERIAL_COMMCONFIG, Size), 0);
        assert_eq!(offset_of!(SERIAL_COMMCONFIG, Version), 4);
        assert_eq!(offset_of!(SERIAL_COMMCONFIG, Reserved), 6);
        assert_eq!(offset_of!(SERIAL_COMMCONFIG, BaudRate), 8);
        assert_eq!(offset_of!(SERIAL_COMMCONFIG, LineControl), 12);
    }

    #[test]
    fn default_values_are_zeroed() {
        let b = SERIAL_BAUD_RATE::default();
        assert_eq!(b.BaudRate, 0);
        let lc = SERIAL_LINE_CONTROL::default();
        assert_eq!((lc.StopBits, lc.Parity, lc.WordLength), (0, 0, 0));
        let t = SERIAL_TIMEOUTS::default();
        assert_eq!(
            (
                t.ReadIntervalTimeout,
                t.ReadTotalTimeoutMultiplier,
                t.ReadTotalTimeoutConstant
            ),
            (0, 0, 0)
        );
        let x = SERIAL_XOFF_COUNTER::default();
        assert_eq!((x.Timeout, x.Counter, x.Usable), (0, 0, 0));
    }
}
