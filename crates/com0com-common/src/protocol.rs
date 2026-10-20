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

impl Com0comPairParams {
    /// Const constructor. The derived `Default` is not a `const fn`, so this is
    /// what `PairContext::new` (a `const fn`) uses to zero the field.
    pub const fn new() -> Self {
        Com0comPairParams {
            port_name: [[0u16; PAIR_PARAMS_NAME_LEN]; 2],
            emu_br: 0,
            emu_overrun: 0,
            emu_noise: 0,
            noise_probability: 0,
            exclusive_mode: 0,
            plugin_mode: 0,
            pin_wiring: [[0u8; 4]; 2],
        }
    }
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
    use core::mem::{offset_of, size_of};

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

    // Wire-contract checks. `setupc` (user mode) and the bus/port drivers size
    // their `DeviceIoControl` buffers from `size_of` and lay fields out per
    // `#[repr(C)]` order; any drift silently corrupts IOCTL payloads. Offsets
    // are asserted (safe code) instead of byte round-trips so the crate keeps
    // its "100% safe Rust" guarantee.
    #[test]
    fn protocol_field_offsets_match_layout() {
        assert_eq!(offset_of!(Com0comPairParams, port_name), 0);
        assert_eq!(offset_of!(Com0comPairParams, emu_br), 128);
        assert_eq!(offset_of!(Com0comPairParams, emu_overrun), 132);
        assert_eq!(offset_of!(Com0comPairParams, emu_noise), 136);
        assert_eq!(offset_of!(Com0comPairParams, noise_probability), 140);
        assert_eq!(offset_of!(Com0comPairParams, exclusive_mode), 144);
        assert_eq!(offset_of!(Com0comPairParams, plugin_mode), 148);
        assert_eq!(offset_of!(Com0comPairParams, pin_wiring), 152);

        assert_eq!(offset_of!(CreatePairRequest, pair_id), 0);
        assert_eq!(offset_of!(CreatePairRequest, params), 4);

        assert_eq!(offset_of!(DeletePairRequest, pair_id), 0);

        assert_eq!(offset_of!(EnumPairEntry, pair_id), 0);
        assert_eq!(offset_of!(EnumPairEntry, params), 4);

        assert_eq!(offset_of!(EnumPairsResponse, count), 0);
        assert_eq!(offset_of!(EnumPairsResponse, entries), 4);

        assert_eq!(offset_of!(GetParamsRequest, pair_id), 0);
        assert_eq!(offset_of!(GetParamsRequest, port_index), 4);

        assert_eq!(offset_of!(SetParamsRequest, pair_id), 0);
        assert_eq!(offset_of!(SetParamsRequest, port_index), 4);
        assert_eq!(offset_of!(SetParamsRequest, params), 8);

        assert_eq!(offset_of!(PortLinkRequest, pair_id), 0);
        assert_eq!(offset_of!(PortLinkRequest, port_index), 4);

        assert_eq!(offset_of!(PortLinkResponse, pair_va), 0);

        assert_eq!(offset_of!(PortUnlinkRequest, pair_id), 0);
        assert_eq!(offset_of!(PortUnlinkRequest, port_index), 4);
        assert_eq!(offset_of!(PortUnlinkRequest, pair_va), 8);
    }

    // `PairContext::new()` is a `const fn` and therefore cannot call the
    // derived `Default`; `Com0comPairParams::new()` must produce the exact same
    // all-zero layout.
    #[test]
    fn pair_params_const_new_matches_default() {
        let a = Com0comPairParams::new();
        let b = Com0comPairParams::default();
        assert_eq!(a.port_name, b.port_name);
        assert_eq!(a.emu_br, b.emu_br);
        assert_eq!(a.emu_overrun, b.emu_overrun);
        assert_eq!(a.emu_noise, b.emu_noise);
        assert_eq!(a.noise_probability, b.noise_probability);
        assert_eq!(a.exclusive_mode, b.exclusive_mode);
        assert_eq!(a.plugin_mode, b.plugin_mode);
        assert_eq!(a.pin_wiring, b.pin_wiring);
        // Everything is zeroed, which is what the bus writes on CREATE_PAIR
        // when `setupc` passes no emu_* flags.
        assert_eq!(a.emu_br, 0);
        assert_eq!(a.emu_noise, 0);
        assert_eq!(a.emu_overrun, 0);
        assert_eq!(a.noise_probability, 0);
        assert_eq!(a.pin_wiring, [[0u8; 4]; 2]);
    }

    #[test]
    fn enum_response_default_is_zeroed() {
        let r = EnumPairsResponse::default();
        assert_eq!(r.count, 0);
        assert!(r
            .entries
            .iter()
            .all(|e| e.pair_id == 0 && e.params.port_name[0][0] == 0));
    }

    #[test]
    fn create_request_field_values_survive_copy() {
        let req = CreatePairRequest {
            pair_id: 0xDEAD_BEEF,
            params: Com0comPairParams {
                emu_br: 1,
                ..Default::default()
            },
        };
        // Copy semantics guarantee a bit-identical replica for the IOCTL send.
        let copy = req;
        assert_eq!(copy.pair_id, 0xDEAD_BEEF);
        assert_eq!(copy.params.emu_br, 1);
        // Default params leave pin_wiring zeroed.
        assert_eq!(copy.params.pin_wiring, [[0u8; 4]; 2]);
    }
}
