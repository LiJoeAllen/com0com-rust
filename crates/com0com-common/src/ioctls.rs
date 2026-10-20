//! `IOCTL_SERIAL_*` codes and com0com-private IOCTLs.
//!
//! The `IOCTL_SERIAL_*` values are reproduced verbatim from the official
//! Microsoft WDK `ntddser.h` (function codes 1..=40). They are computed with
//! [`ctl_code`] instead of hard-coded so there is no arithmetic drift.
//!
//! `METHOD_BUFFERED` + `FILE_ANY_ACCESS` is what every real Windows serial
//! driver (and the Win32 Comm API) uses, so the values below MUST match the
//! inbox `serial.sys` exactly or applications will send the wrong control code.

pub const FILE_DEVICE_SERIAL_PORT: u32 = 0x0000_001b;
pub const FILE_DEVICE_COM0COM: u32 = 0x0000_9001; // private device type

pub const METHOD_BUFFERED: u32 = 0;
pub const METHOD_IN_DIRECT: u32 = 1;
pub const METHOD_OUT_DIRECT: u32 = 2;
pub const METHOD_NEITHER: u32 = 3;

pub const FILE_ANY_ACCESS: u32 = 0;
pub const FILE_READ_ACCESS: u32 = 0x0001;
pub const FILE_WRITE_ACCESS: u32 = 0x0002;

/// Mirrors the WDK `CTL_CODE` macro.
#[inline]
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

macro_rules! serial_ioctl {
    ($name:ident, $fn:expr) => {
        pub const $name: u32 = ctl_code(
            FILE_DEVICE_SERIAL_PORT,
            $fn,
            METHOD_BUFFERED,
            FILE_ANY_ACCESS,
        );
    };
}

// ---- Official Microsoft WDK ntddser.h function codes (1..=40) ----
serial_ioctl!(IOCTL_SERIAL_SET_BAUD_RATE, 1);
serial_ioctl!(IOCTL_SERIAL_SET_QUEUE_SIZE, 2);
serial_ioctl!(IOCTL_SERIAL_SET_LINE_CONTROL, 3);
serial_ioctl!(IOCTL_SERIAL_SET_BREAK_ON, 4);
serial_ioctl!(IOCTL_SERIAL_SET_BREAK_OFF, 5);
serial_ioctl!(IOCTL_SERIAL_IMMEDIATE_CHAR, 6);
serial_ioctl!(IOCTL_SERIAL_SET_TIMEOUTS, 7);
serial_ioctl!(IOCTL_SERIAL_GET_TIMEOUTS, 8);
serial_ioctl!(IOCTL_SERIAL_SET_DTR, 9);
serial_ioctl!(IOCTL_SERIAL_CLR_DTR, 10);
serial_ioctl!(IOCTL_SERIAL_RESET_DEVICE, 11);
serial_ioctl!(IOCTL_SERIAL_SET_RTS, 12);
serial_ioctl!(IOCTL_SERIAL_CLR_RTS, 13);
serial_ioctl!(IOCTL_SERIAL_SET_XOFF, 14);
serial_ioctl!(IOCTL_SERIAL_SET_XON, 15);
serial_ioctl!(IOCTL_SERIAL_GET_WAIT_MASK, 16);
serial_ioctl!(IOCTL_SERIAL_SET_WAIT_MASK, 17);
serial_ioctl!(IOCTL_SERIAL_WAIT_ON_MASK, 18);
serial_ioctl!(IOCTL_SERIAL_PURGE, 19);
serial_ioctl!(IOCTL_SERIAL_GET_BAUD_RATE, 20);
serial_ioctl!(IOCTL_SERIAL_GET_LINE_CONTROL, 21);
serial_ioctl!(IOCTL_SERIAL_GET_CHARS, 22);
serial_ioctl!(IOCTL_SERIAL_SET_CHARS, 23);
serial_ioctl!(IOCTL_SERIAL_GET_HANDFLOW, 24);
serial_ioctl!(IOCTL_SERIAL_SET_HANDFLOW, 25);
serial_ioctl!(IOCTL_SERIAL_GET_MODEMSTATUS, 26);
serial_ioctl!(IOCTL_SERIAL_GET_COMMSTATUS, 27);
serial_ioctl!(IOCTL_SERIAL_XOFF_COUNTER, 28);
serial_ioctl!(IOCTL_SERIAL_GET_PROPERTIES, 29);
serial_ioctl!(IOCTL_SERIAL_GET_DTRRTS, 30);
serial_ioctl!(IOCTL_SERIAL_LSRMST_INSERT, 31);
serial_ioctl!(IOCTL_SERIAL_CONFIG_SIZE, 32);
serial_ioctl!(IOCTL_SERIAL_GET_COMMCONFIG, 33);
serial_ioctl!(IOCTL_SERIAL_SET_COMMCONFIG, 34);
serial_ioctl!(IOCTL_SERIAL_GET_STATS, 35);
serial_ioctl!(IOCTL_SERIAL_CLEAR_STATS, 36);
serial_ioctl!(IOCTL_SERIAL_GET_MODEM_CONTROL, 37);
serial_ioctl!(IOCTL_SERIAL_SET_MODEM_CONTROL, 38);
serial_ioctl!(IOCTL_SERIAL_SET_FIFO_CONTROL, 39);
serial_ioctl!(IOCTL_SERIAL_APPLY_DEFAULT_CONFIGURATION, 40);

// ---- com0com private IOCTLs (bus control device) ----
// These travel to the bus driver's control device interface
// (GUID_COM0COM_BUS). Some are issued by the user-mode `setupc` tool, some by
// the port driver (kernel mode) to attach to its shared pair context.

pub const IOCTL_COM0COM_CREATE_PAIR: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x800,
    METHOD_BUFFERED,
    FILE_WRITE_ACCESS,
);
pub const IOCTL_COM0COM_DELETE_PAIR: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x801,
    METHOD_BUFFERED,
    FILE_WRITE_ACCESS,
);
pub const IOCTL_COM0COM_ENUM_PAIRS: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x802,
    METHOD_BUFFERED,
    FILE_READ_ACCESS,
);
pub const IOCTL_COM0COM_GET_PARAMS: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x803,
    METHOD_BUFFERED,
    FILE_READ_ACCESS,
);
pub const IOCTL_COM0COM_SET_PARAMS: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x804,
    METHOD_BUFFERED,
    FILE_WRITE_ACCESS,
);

/// Port driver -> bus: obtain the kernel VA of the pair context for
/// (pair_id, port_index) and bump its reference count.
pub const IOCTL_COM0COM_PORT_LINK: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x810,
    METHOD_BUFFERED,
    FILE_WRITE_ACCESS,
);
/// Port driver -> bus: drop the reference obtained with PORT_LINK.
pub const IOCTL_COM0COM_PORT_UNLINK: u32 = ctl_code(
    FILE_DEVICE_COM0COM,
    0x811,
    METHOD_BUFFERED,
    FILE_WRITE_ACCESS,
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `IOCTL_SERIAL_*` MUST equal `CTL_CODE(FILE_DEVICE_SERIAL_PORT,
    /// fn, METHOD_BUFFERED, FILE_ANY_ACCESS)`. Encoding the function code next
    /// to the constant guards against an accidental renumber or a macro drift.
    #[test]
    fn serial_ioctls_match_wdk_formula() {
        assert_eq!(
            IOCTL_SERIAL_SET_BAUD_RATE,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 1, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_SET_QUEUE_SIZE,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 2, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_SET_LINE_CONTROL,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 3, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_SET_BREAK_ON,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 4, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_SET_BREAK_OFF,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 5, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_IMMEDIATE_CHAR,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 6, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_SET_TIMEOUTS,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 7, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_GET_TIMEOUTS,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 8, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_SET_DTR,
            ctl_code(FILE_DEVICE_SERIAL_PORT, 9, METHOD_BUFFERED, FILE_ANY_ACCESS)
        );
        assert_eq!(
            IOCTL_SERIAL_CLR_DTR,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                10,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_RESET_DEVICE,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                11,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_RTS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                12,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_CLR_RTS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                13,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_XOFF,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                14,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_XON,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                15,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_WAIT_MASK,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                16,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_WAIT_MASK,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                17,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_WAIT_ON_MASK,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                18,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_PURGE,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                19,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_BAUD_RATE,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                20,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_LINE_CONTROL,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                21,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_CHARS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                22,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_CHARS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                23,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_HANDFLOW,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                24,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_HANDFLOW,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                25,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_MODEMSTATUS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                26,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_COMMSTATUS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                27,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_XOFF_COUNTER,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                28,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_PROPERTIES,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                29,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_DTRRTS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                30,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_LSRMST_INSERT,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                31,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_CONFIG_SIZE,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                32,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_COMMCONFIG,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                33,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_COMMCONFIG,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                34,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_STATS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                35,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_CLEAR_STATS,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                36,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_GET_MODEM_CONTROL,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                37,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_MODEM_CONTROL,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                38,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_SET_FIFO_CONTROL,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                39,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(
            IOCTL_SERIAL_APPLY_DEFAULT_CONFIGURATION,
            ctl_code(
                FILE_DEVICE_SERIAL_PORT,
                40,
                METHOD_BUFFERED,
                FILE_ANY_ACCESS
            )
        );
    }

    /// Spot-checks against the documented hex values in WDK `ntddser.h`. These
    /// are the values real `serial.sys` answers, so an application compiled
    /// against the WDK sends exactly these codes.
    #[test]
    fn serial_ioctls_match_wdk_hex() {
        assert_eq!(IOCTL_SERIAL_SET_BAUD_RATE, 0x001B_0004);
        assert_eq!(IOCTL_SERIAL_GET_BAUD_RATE, 0x001B_0050);
        assert_eq!(IOCTL_SERIAL_GET_MODEMSTATUS, 0x001B_0068);
        assert_eq!(IOCTL_SERIAL_SET_MODEM_CONTROL, 0x001B_0098);
        assert_eq!(IOCTL_SERIAL_GET_COMMCONFIG, 0x001B_00B4); // fn 33 -> 33<<2=0x84
        assert_eq!(IOCTL_SERIAL_SET_COMMCONFIG, 0x001B_00B8); // fn 34 -> 34<<2=0x88
    }

    /// The private com0com bus IOCTLs use a distinct device type and access
    /// level, and must not collide with the inbox serial codes.
    #[test]
    fn private_ioctls_match_formula() {
        assert_eq!(
            IOCTL_COM0COM_CREATE_PAIR,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x800,
                METHOD_BUFFERED,
                FILE_WRITE_ACCESS
            )
        );
        assert_eq!(
            IOCTL_COM0COM_DELETE_PAIR,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x801,
                METHOD_BUFFERED,
                FILE_WRITE_ACCESS
            )
        );
        assert_eq!(
            IOCTL_COM0COM_ENUM_PAIRS,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x802,
                METHOD_BUFFERED,
                FILE_READ_ACCESS
            )
        );
        assert_eq!(
            IOCTL_COM0COM_GET_PARAMS,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x803,
                METHOD_BUFFERED,
                FILE_READ_ACCESS
            )
        );
        assert_eq!(
            IOCTL_COM0COM_SET_PARAMS,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x804,
                METHOD_BUFFERED,
                FILE_WRITE_ACCESS
            )
        );
        assert_eq!(
            IOCTL_COM0COM_PORT_LINK,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x810,
                METHOD_BUFFERED,
                FILE_WRITE_ACCESS
            )
        );
        assert_eq!(
            IOCTL_COM0COM_PORT_UNLINK,
            ctl_code(
                FILE_DEVICE_COM0COM,
                0x811,
                METHOD_BUFFERED,
                FILE_WRITE_ACCESS
            )
        );
    }

    #[test]
    fn device_types_and_methods_are_sane() {
        // Private device type must differ from the inbox serial port type so
        // the two control namespaces never alias.
        assert_ne!(FILE_DEVICE_COM0COM, FILE_DEVICE_SERIAL_PORT);
        assert_eq!(FILE_DEVICE_SERIAL_PORT, 0x0000_001b);
        assert_eq!(FILE_DEVICE_COM0COM, 0x0000_9001);
        assert_eq!(METHOD_BUFFERED, 0);
        assert_eq!(FILE_ANY_ACCESS, 0);
        assert_eq!(FILE_READ_ACCESS, 0x0001);
        assert_eq!(FILE_WRITE_ACCESS, 0x0002);
    }

    #[test]
    fn ctl_code_is_injective_per_field() {
        // The low 2 bits are the method; function lives in bits 2..14; access
        // in bits 14..16; device type in bits 16..32. Exercise the encoding.
        assert_eq!(ctl_code(0x1b, 1, 0, 0), 0x001B_0004);
        assert_eq!(ctl_code(0x9001, 0x800, 0, 2), IOCTL_COM0COM_CREATE_PAIR);
        // Distinguish METHOD_BUFFERED (0) from METHOD_NEITHER (3) on the same fn.
        assert_ne!(
            ctl_code(FILE_DEVICE_COM0COM, 0x800, METHOD_BUFFERED, 2),
            ctl_code(FILE_DEVICE_COM0COM, 0x800, METHOD_NEITHER, 2)
        );
    }
}
