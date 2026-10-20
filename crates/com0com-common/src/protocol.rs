//! Wire structures for the com0com private IOCTLs (bus control device).
//!
//! These travel in the `METHOD_BUFFERED` system buffer between the user-mode
//! `setupc` tool, the bus driver, and the port driver. All are `#[repr(C)]`
//! with fixed layout and no pointers except the kernel VA returned by
//! `PORT_LINK`.

pub const PAIR_PARAMS_NAME_LEN: usize = 32;
pub const MAX_ENUM_PAIRS: usize = 64;

/// Per-pair configuration. Mirrors com0com's registry Parameters key
/// (`HKLM\...\com0com\Parameters\{PortName}`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Com0comPairParams {
    /// Wide-char port names for port A (index 0) and port B (index 1),
    /// e.g. "COM15". Empty string => let the COM Name Arbiter assign one.
    pub port_name: [[u16; PAIR_PARAMS_NAME_LEN]; 2],
    pub emu_br: u32,            // 0/1: baud-rate (timing) emulation
    pub emu_overrun: u32,       // 0/1: simulate receive buffer overrun
    pub emu_noise: u32,         // 0/1: inject frame corruption
    pub noise_probability: u32, // 0..10000 (0.01% units) when emu_noise
    pub exclusive_mode: u32,    // 0/1: hide port while open by another app
    pub plugin_mode: u32,       // 0/1: hide port until its peer is opened
    /// Null-modem pin wiring for A and B (RTS/DTR/OUT1/OUT2 -> peer input).
    pub pin_wiring: [[u8; 4]; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CreatePairRequest {
    pub pair_id: u32,
    pub params: Com0comPairParams,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DeletePairRequest {
    pub pair_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EnumPairEntry {
    pub pair_id: u32,
    pub params: Com0comPairParams,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EnumPairsResponse {
    pub count: u32,
    pub entries: [EnumPairEntry; MAX_ENUM_PAIRS],
}

impl Default for EnumPairsResponse {
    fn default() -> Self {
        EnumPairsResponse {
            count: 0,
            entries: [EnumPairEntry::default(); MAX_ENUM_PAIRS],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GetParamsRequest {
    pub pair_id: u32,
    pub port_index: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SetParamsRequest {
    pub pair_id: u32,
    pub port_index: u8,
    pub params: Com0comPairParams,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PortLinkRequest {
    pub pair_id: u32,
    pub port_index: u8, // 0 = A, 1 = B
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PortLinkResponse {
    /// Kernel VA of the `PairContext`. Valid for the calling driver because
    /// kernel mode shares one address space.
    pub pair_va: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PortUnlinkRequest {
    pub pair_id: u32,
    pub port_index: u8,
    pub pair_va: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{size_of, transmute};

    #[test]
    fn constants_are_sane() {
        assert_eq!(PAIR_PARAMS_NAME_LEN, 32);
        assert_eq!(MAX_ENUM_PAIRS, 64);
    }

    // These sizes are part of the wire contract: `setupc` (user mode) and the
    // bus/port drivers size their `DeviceIoControl` buffers from them. Any drift
    // silently corrupts the IOCTL payloads.
    #[test]
    fn struct_sizes_are_stable() {
        assert_eq!(size_of::<Com0comPairParams>(), 160);
        assert_eq!(size_of::<CreatePairRequest>(), 164);
        assert_eq!(size_of::<DeletePairRequest>(), 4);
        assert_eq!(size_of::<EnumPairEntry>(), 164);
        assert_eq!(size_of::<EnumPairsResponse>(), 10500);
        assert_eq!(size_of::<GetParamsRequest>(), 8);
        assert_eq!(size_of::<SetParamsRequest>(), 168);
        assert_eq!(size_of::<PortLinkRequest>(), 8);
        assert_eq!(size_of::<PortLinkResponse>(), 8);
        assert_eq!(size_of::<PortUnlinkRequest>(), 16);
    }

    /// Reinterpret `T` as bytes and back; the bytes must be identical. This is
    /// exactly what happens across the `METHOD_BUFFERED` system buffer between
    /// `setupc`, the bus driver, and the port driver.
    fn roundtrip<T: Copy>() {
        let mut buf = [0u8; size_of::<T>()];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(0x55);
        }
        let original: T = unsafe { transmute(buf) };
        let back: [u8; size_of::<T>()] = unsafe { transmute(original) };
        assert_eq!(buf, back, "byte layout must survive a round trip");
    }

    #[test]
    fn wire_structs_roundtrip() {
        roundtrip::<Com0comPairParams>();
        roundtrip::<CreatePairRequest>();
        roundtrip::<DeletePairRequest>();
        roundtrip::<EnumPairEntry>();
        roundtrip::<EnumPairsResponse>();
        roundtrip::<GetParamsRequest>();
        roundtrip::<SetParamsRequest>();
        roundtrip::<PortLinkRequest>();
        roundtrip::<PortLinkResponse>();
        roundtrip::<PortUnlinkRequest>();
    }

    #[test]
    fn enum_response_default_is_zeroed() {
        let r = EnumPairsResponse::default();
        assert_eq!(r.count, 0);
        let bytes: [u8; size_of::<EnumPairsResponse>()] = unsafe { transmute(r) };
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn create_request_layout_carries_params() {
        // A constructed request must place pair_id before params, matching the
        // field order the bus driver reads.
        let req = CreatePairRequest {
            pair_id: 0xDEAD_BEEF,
            params: Com0comPairParams {
                emu_br: 1,
                ..Default::default()
            },
        };
        let bytes: [u8; size_of::<CreatePairRequest>()] = unsafe { transmute(req) };
        // pair_id is the first 4 bytes, little-endian.
        assert_eq!(&bytes[0..4], &[0xEF, 0xBE, 0xAD, 0xDE]);
        // emu_br is the first u32 of params, which begins at offset 4.
        assert_eq!(&bytes[4..8], &[1, 0, 0, 0]);
    }
}
