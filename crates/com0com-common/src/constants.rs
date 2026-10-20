//! Flag, mask and identifier constants for the serial emulation and for the
//! com0com private interfaces. Values are transcribed from the official WDK
//! `ntddser.h` and `devpkey`/setup APIs.

// ---------- HANDFLOW: ControlHandShake (SERIAL_HANDFLOW.ControlHandShake) ----------
pub const SERIAL_DTR_MASK: u32 = 0x0000_0003;
pub const SERIAL_DTR_CONTROL: u32 = 0x0000_0001;
pub const SERIAL_DTR_HANDSHAKE: u32 = 0x0000_0002;
pub const SERIAL_CTS_HANDSHAKE: u32 = 0x0000_0008;
pub const SERIAL_DSR_HANDSHAKE: u32 = 0x0000_0010;
pub const SERIAL_DCD_HANDSHAKE: u32 = 0x0000_0020;
pub const SERIAL_OUT_HANDSHAKEMASK: u32 = 0x0000_0038;
pub const SERIAL_DSR_SENSITIVITY: u32 = 0x0000_0040;
pub const SERIAL_ERROR_ABORT: u32 = 0x8000_0000;

// ---------- HANDFLOW: FlowReplace (SERIAL_HANDFLOW.FlowReplace) ----------
pub const SERIAL_AUTO_TRANSMIT: u32 = 0x0000_0001;
pub const SERIAL_AUTO_RECEIVE: u32 = 0x0000_0002;
pub const SERIAL_ERROR_CHAR: u32 = 0x0000_0004;
pub const SERIAL_NULL_STRIPPING: u32 = 0x0000_0008;
pub const SERIAL_BREAK_CHAR: u32 = 0x0000_0010;
pub const SERIAL_RTS_MASK: u32 = 0x0000_00C0;
pub const SERIAL_RTS_CONTROL: u32 = 0x0000_0040;
pub const SERIAL_RTS_HANDSHAKE: u32 = 0x0000_0080;
pub const SERIAL_TRANSMIT_TOGGLE: u32 = 0x0000_00C0;
pub const SERIAL_XOFF_CONTINUE: u32 = 0x8000_0000;

// ---------- Line control constants (SERIAL_LINE_CONTROL) ----------
pub const STOP_BIT_1: u8 = 0x00;
pub const STOP_BITS_1_5: u8 = 0x01;
pub const STOP_BITS_2: u8 = 0x02;

pub const NO_PARITY: u8 = 0x00;
pub const ODD_PARITY: u8 = 0x01;
pub const EVEN_PARITY: u8 = 0x02;
pub const MARK_PARITY: u8 = 0x03;
pub const SPACE_PARITY: u8 = 0x04;

// ---------- Wait mask bits (SERIAL_EV_*) ----------
pub const SERIAL_EV_RXCHAR: u32 = 0x0001;
pub const SERIAL_EV_RXFLAG: u32 = 0x0002;
pub const SERIAL_EV_TXEMPTY: u32 = 0x0004;
pub const SERIAL_EV_CTS: u32 = 0x0008;
pub const SERIAL_EV_DSR: u32 = 0x0010;
pub const SERIAL_EV_RLSD: u32 = 0x0020;
pub const SERIAL_EV_BREAK: u32 = 0x0040;
pub const SERIAL_EV_ERR: u32 = 0x0080;
pub const SERIAL_EV_RING: u32 = 0x0100;
pub const SERIAL_EV_PERR: u32 = 0x0200;
pub const SERIAL_EV_RX80FULL: u32 = 0x0400;
pub const SERIAL_EV_EVENT1: u32 = 0x0800;
pub const SERIAL_EV_EVENT2: u32 = 0x1000;

// ---------- Modem status bits returned by GET_MODEMSTATUS ----------
pub const SERIAL_DTR_STATE: u8 = 0x01;
pub const SERIAL_RTS_STATE: u8 = 0x02;
pub const SERIAL_CTS_STATE: u8 = 0x10;
pub const SERIAL_DSR_STATE: u8 = 0x20;
pub const SERIAL_RI_STATE: u8 = 0x40;
pub const SERIAL_DCD_STATE: u8 = 0x80;

// ---------- Error bits (SERIAL_STATUS.Errors / CE_*) ----------
pub const CE_RXOVER: u32 = 0x0001;
pub const CE_OVERRUN: u32 = 0x0002;
pub const CE_RXPARITY: u32 = 0x0004;
pub const CE_FRAME: u32 = 0x0008;
pub const CE_BREAK: u32 = 0x0010;
pub const CE_TXFULL: u32 = 0x0100;

// ---------- SERIAL_COMMPROP ProvCapabilities / Settable* ----------
pub const SERIAL_SP_UNSPECIFIED: u32 = 0x0000_0000;
pub const SERIAL_SP_RS232: u32 = 0x0000_0001;

pub const SERIAL_PCF_DTRDSR: u32 = 0x0001;
pub const SERIAL_PCF_RTSCTS: u32 = 0x0002;
pub const SERIAL_PCF_XONXOFF: u32 = 0x0004;
pub const SERIAL_PCF_SETXCHAR: u32 = 0x0008;
pub const SERIAL_PCF_TOTALTIMEOUTS: u32 = 0x0020;
pub const SERIAL_PCF_INTTIMEOUTS: u32 = 0x0080;
pub const SERIAL_PCF_SPECIALCHARS: u32 = 0x0100;
pub const SERIAL_PCF_16BITMODE: u32 = 0x0200;

pub const SERIAL_SP_PARITY: u32 = 0x0001;
pub const SERIAL_SP_BAUD: u32 = 0x0002;
pub const SERIAL_SP_DATABITS: u32 = 0x0004;
pub const SERIAL_SP_STOPBITS: u32 = 0x0008;
pub const SERIAL_SP_HANDSHAKING: u32 = 0x0010;
pub const SERIAL_SP_PARITY_CHECK: u32 = 0x0020;
pub const SERIAL_SP_CARRIER_DETECT: u32 = 0x0040;

pub const SERIAL_BAUD_USER: u32 = 0x1000_0000;

// SERIAL_PCF and SERIAL_SP combined mask used by GetProperties.
pub const COM0COM_PROV_CAPABILITIES: u32 = SERIAL_PCF_DTRDSR
    | SERIAL_PCF_RTSCTS
    | SERIAL_PCF_XONXOFF
    | SERIAL_PCF_SETXCHAR
    | SERIAL_PCF_TOTALTIMEOUTS
    | SERIAL_PCF_INTTIMEOUTS
    | SERIAL_PCF_SPECIALCHARS;
pub const COM0COM_SETTABLE_PARAMS: u32 = SERIAL_SP_PARITY
    | SERIAL_SP_BAUD
    | SERIAL_SP_DATABITS
    | SERIAL_SP_STOPBITS
    | SERIAL_SP_HANDSHAKING
    | SERIAL_SP_PARITY_CHECK
    | SERIAL_SP_CARRIER_DETECT;
pub const COM0COM_SETTABLE_DATA: u16 = 0x001F; // data bits 5..=9
pub const COM0COM_SETTABLE_STOP_PARITY: u16 = 0x0107; // 1/1.5/2 stop, all parities

// ---------- Device interface GUIDs (little-endian GUID bytes) ----------
// GUID_DEVINTERFACE_COMPORT
pub const GUID_DEVINTERFACE_COMPORT: [u8; 16] = [
    0xe0, 0xd1, 0xe0, 0x86, 0x89, 0x80, 0xd0, 0x11, 0x9c, 0xe4, 0x08, 0x00, 0x3e, 0x30, 0x1f, 0x73,
];
// Private: com0com bus control device (setup tool + port driver talk here).
pub const GUID_COM0COM_BUS: [u8; 16] = [
    0xc0, 0x0c, 0x0c, 0x90, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
];
/// Byte form (alias) for `guid_from_bytes`.
pub const GUID_COM0COM_BUS_BYTES: [u8; 16] = GUID_COM0COM_BUS;

// ---------- PnP hardware IDs ----------
// The bus FDO and the two port roles. Pair instances are distinguished by PnP
// instance IDs (WDF child identification description), so the hardware IDs are
// constant across all pairs.
pub const BUS_HW_ID: &str = "COM0COM\\BUS";
pub const PORT_A_HW_ID: &str = "COM0COM\\PORT_A";
pub const PORT_B_HW_ID: &str = "COM0COM\\PORT_B";

// ---------- Default null-modem pin wiring (RTS->CTS, DTR->DSR, OUT1->DCD, OUT2->RI) ----------
// Index 0 = port A, index 1 = port B. Each entry describes how THIS port's
// output lines map to the PEER's input (modem-status) lines.
pub const DEFAULT_PIN_WIRING: [[u8; 4]; 2] = [
    // A's RTS/DTR/OUT1/OUT2 -> B sees as CTS/DSR/DCD/RI
    [
        SERIAL_CTS_STATE,
        SERIAL_DSR_STATE,
        SERIAL_DCD_STATE,
        SERIAL_RI_STATE,
    ],
    [
        SERIAL_CTS_STATE,
        SERIAL_DSR_STATE,
        SERIAL_DCD_STATE,
        SERIAL_RI_STATE,
    ],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_comport_matches_wdk() {
        // GUID_DEVINTERFACE_COMPORT = {86E0D1E0-8089-11D0-9CE4-08003E301F73}
        assert_eq!(
            GUID_DEVINTERFACE_COMPORT,
            [
                0xe0, 0xd1, 0xe0, 0x86, 0x89, 0x80, 0xd0, 0x11, 0x9c, 0xe4, 0x08, 0x00, 0x3e, 0x30,
                0x1f, 0x73
            ]
        );
    }

    #[test]
    fn guid_bus_bytes_alias() {
        assert_eq!(GUID_COM0COM_BUS_BYTES, GUID_COM0COM_BUS);
        // Private bus GUID must differ from the inbox COMPORT interface GUID.
        assert_ne!(GUID_COM0COM_BUS, GUID_DEVINTERFACE_COMPORT);
    }

    #[test]
    fn default_pin_wiring_maps_all_lines() {
        for role in 0..2 {
            assert_eq!(DEFAULT_PIN_WIRING[role][0], SERIAL_CTS_STATE);
            assert_eq!(DEFAULT_PIN_WIRING[role][1], SERIAL_DSR_STATE);
            assert_eq!(DEFAULT_PIN_WIRING[role][2], SERIAL_DCD_STATE);
            assert_eq!(DEFAULT_PIN_WIRING[role][3], SERIAL_RI_STATE);
        }
    }

    #[test]
    fn capability_and_settable_masks_are_consistent() {
        assert_eq!(
            COM0COM_PROV_CAPABILITIES,
            SERIAL_PCF_DTRDSR
                | SERIAL_PCF_RTSCTS
                | SERIAL_PCF_XONXOFF
                | SERIAL_PCF_SETXCHAR
                | SERIAL_PCF_TOTALTIMEOUTS
                | SERIAL_PCF_INTTIMEOUTS
                | SERIAL_PCF_SPECIALCHARS
        );
        assert_eq!(
            COM0COM_SETTABLE_PARAMS,
            SERIAL_SP_PARITY
                | SERIAL_SP_BAUD
                | SERIAL_SP_DATABITS
                | SERIAL_SP_STOPBITS
                | SERIAL_SP_HANDSHAKING
                | SERIAL_SP_PARITY_CHECK
                | SERIAL_SP_CARRIER_DETECT
        );
        // Data bits 5..=9 -> bitmask 0b11111 = 0x1F.
        assert_eq!(COM0COM_SETTABLE_DATA, 0x001F);
        // 1/1.5/2 stop bits + all parities.
        assert_eq!(COM0COM_SETTABLE_STOP_PARITY, 0x0107);
    }

    #[test]
    fn hardware_ids_are_distinct() {
        assert_eq!(BUS_HW_ID, "COM0COM\\BUS");
        assert_eq!(PORT_A_HW_ID, "COM0COM\\PORT_A");
        assert_eq!(PORT_B_HW_ID, "COM0COM\\PORT_B");
        assert_ne!(PORT_A_HW_ID, PORT_B_HW_ID);
    }

    #[test]
    fn handflow_mask_constants() {
        assert_eq!(SERIAL_DTR_MASK, 0x0000_0003);
        assert_eq!(SERIAL_OUT_HANDSHAKEMASK, 0x0000_0038);
        assert_eq!(SERIAL_RTS_MASK, 0x0000_00C0);
        assert_eq!(SERIAL_TRANSMIT_TOGGLE, 0x0000_00C0);
        assert_eq!(SERIAL_ERROR_ABORT, 0x8000_0000);
        assert_eq!(SERIAL_XOFF_CONTINUE, 0x8000_0000);
    }

    #[test]
    fn modem_status_and_error_bits() {
        assert_eq!(SERIAL_CTS_STATE, 0x10);
        assert_eq!(SERIAL_DSR_STATE, 0x20);
        assert_eq!(SERIAL_RI_STATE, 0x40);
        assert_eq!(SERIAL_DCD_STATE, 0x80);
        assert_eq!(SERIAL_DTR_STATE, 0x01);
        assert_eq!(SERIAL_RTS_STATE, 0x02);
        assert_eq!(CE_RXOVER, 0x0001);
        assert_eq!(CE_FRAME, 0x0008);
        assert_eq!(CE_TXFULL, 0x0100);
    }

    #[test]
    fn wait_mask_bits_are_distinct_powers_of_two() {
        let masks = [
            SERIAL_EV_RXCHAR,
            SERIAL_EV_RXFLAG,
            SERIAL_EV_TXEMPTY,
            SERIAL_EV_CTS,
            SERIAL_EV_DSR,
            SERIAL_EV_RLSD,
            SERIAL_EV_BREAK,
            SERIAL_EV_ERR,
            SERIAL_EV_RING,
            SERIAL_EV_PERR,
            SERIAL_EV_RX80FULL,
            SERIAL_EV_EVENT1,
            SERIAL_EV_EVENT2,
        ];
        // All distinct and each a single set bit.
        for (i, &m) in masks.iter().enumerate() {
            assert_eq!(m & (m - 1), 0, "wait mask must be a single bit");
            for (j, &n) in masks.iter().enumerate() {
                if i != j {
                    assert_ne!(m, n);
                }
            }
        }
    }
}
