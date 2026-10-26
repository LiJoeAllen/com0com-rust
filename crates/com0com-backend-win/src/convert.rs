//! Conversion between the platform-neutral domain types and the bus wire
//! structs. Kept free of Win32 so it is unit-tested on every host.

use com0com_common::protocol::{Com0comPairParams, PAIR_PARAMS_NAME_LEN};
use com0com_core::{PairParams, PinWiring, PortName, SdkError, MAX_PORT_NAME_CHARS};

/// Decode a NUL-terminated UTF-16 port name from the wire struct.
pub(crate) fn wide_to_name(buf: &[u16]) -> Result<PortName, SdkError> {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let name = String::from_utf16_lossy(&buf[..end]);
    PortName::new(name)
        .map_err(|e| SdkError::Internal(format!("driver returned an invalid port name: {e}")))
}

/// Encode a port name into the fixed 32-unit wire buffer.
pub(crate) fn name_to_wide(name: &PortName) -> Result<[u16; PAIR_PARAMS_NAME_LEN], SdkError> {
    let units: Vec<u16> = name.as_str().encode_utf16().collect();
    if units.len() > MAX_PORT_NAME_CHARS {
        return Err(SdkError::InvalidArgument(format!(
            "port name is {} UTF-16 units, maximum is {MAX_PORT_NAME_CHARS}",
            units.len()
        )));
    }
    let mut out = [0u16; PAIR_PARAMS_NAME_LEN];
    out[..units.len()].copy_from_slice(&units);
    Ok(out)
}

pub(crate) fn wire_to_params(wire: &Com0comPairParams) -> Result<PairParams, SdkError> {
    if wire.noise_probability > 100_000_000 {
        return Err(SdkError::Internal(format!(
            "driver returned noise_probability {} outside 0..=100000000",
            wire.noise_probability
        )));
    }
    Ok(PairParams {
        port_a: wide_to_name(&wire.port_name[0])?,
        port_b: wide_to_name(&wire.port_name[1])?,
        baud_emu: wire.emu_br != 0,
        overrun: wire.emu_overrun != 0,
        noise_enabled: wire.emu_noise != 0,
        noise_probability: wire.noise_probability,
        all_data_bits: wire.all_data_bits != 0,
        exclusive: wire.exclusive_mode != 0,
        plugin: wire.plugin_mode != 0,
        pin_wiring_a: PinWiring {
            lines: wire.pin_wiring[0],
        },
        pin_wiring_b: PinWiring {
            lines: wire.pin_wiring[1],
        },
    })
}

pub(crate) fn params_to_wire(params: &PairParams) -> Result<Com0comPairParams, SdkError> {
    Ok(Com0comPairParams {
        port_name: [name_to_wide(&params.port_a)?, name_to_wide(&params.port_b)?],
        emu_br: params.baud_emu as u32,
        emu_overrun: params.overrun as u32,
        emu_noise: params.noise_enabled as u32,
        noise_probability: params.noise_probability,
        exclusive_mode: params.exclusive as u32,
        plugin_mode: params.plugin as u32,
        pin_wiring: [params.pin_wiring_a.lines, params.pin_wiring_b.lines],
        all_data_bits: params.all_data_bits as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_string_helpers_roundtrip() {
        let name = PortName::new("COM12").unwrap();
        let wire = name_to_wide(&name).unwrap();
        assert_eq!(wire[5], 0);
        assert_eq!(wide_to_name(&wire).unwrap(), name);

        let raw: [u16; 6] = [b'A' as u16, b'B' as u16, 0, b'X' as u16, b'Y' as u16, 0];
        assert_eq!(wide_to_name(&raw).unwrap().as_str(), "AB");
    }

    #[test]
    fn name_to_wide_rejects_over_31_utf16_units() {
        let name = PortName::new("x".repeat(MAX_PORT_NAME_CHARS)).unwrap();
        assert!(name_to_wide(&name).is_ok());
        // Surrogate pairs count as one char but two UTF-16 units.
        let name = PortName::new("\u{1F600}".repeat(16)).unwrap();
        assert!(name_to_wide(&name).is_err());
    }

    #[test]
    fn params_roundtrip_through_wire() {
        let params = PairParams {
            port_a: PortName::new("COM15").unwrap(),
            port_b: PortName::new("COM16").unwrap(),
            baud_emu: true,
            overrun: true,
            noise_enabled: true,
            noise_probability: 3_000_000,
            all_data_bits: true,
            exclusive: true,
            plugin: true,
            pin_wiring_a: PinWiring {
                lines: [1, 2, 3, 4],
            },
            pin_wiring_b: PinWiring::NONE,
        };
        let wire = params_to_wire(&params).unwrap();
        assert_eq!(wire.emu_br, 1);
        assert_eq!(wire.noise_probability, 3_000_000);
        assert_eq!(wire.all_data_bits, 1);
        assert_eq!(wire.pin_wiring, [[1, 2, 3, 4], [0, 0, 0, 0]]);
        assert_eq!(wire_to_params(&wire).unwrap(), params);
    }

    #[test]
    fn rejects_out_of_range_wire_noise_probability() {
        let mut wire = Com0comPairParams {
            noise_probability: 100_000_001,
            ..Com0comPairParams::default()
        };
        assert!(matches!(wire_to_params(&wire), Err(SdkError::Internal(_))));
        wire.noise_probability = u32::MAX;
        assert!(matches!(wire_to_params(&wire), Err(SdkError::Internal(_))));
    }
}
