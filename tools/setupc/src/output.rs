//! Human-readable setupc output. Kept separate from command dispatch so the
//! exact text contract is unit-tested.

use com0com_core::{Pair, PairId, PairParams};

pub const LIST_HEADER: &str =
    "Pair  PortA      PortB      Flags\n----  ---------  ---------  ----------------------------";

pub fn describe_flags(p: &PairParams) -> String {
    let mut f: Vec<String> = Vec::new();
    if p.baud_emu {
        f.push("baud-emu".to_string());
    }
    if p.overrun {
        f.push("overrun".to_string());
    }
    if p.noise_enabled {
        f.push(format!("noise({}%)", p.noise_probability / 100));
    }
    if p.exclusive {
        f.push("exclusive".to_string());
    }
    if p.plugin {
        f.push("plugin".to_string());
    }
    if f.is_empty() {
        "default".to_string()
    } else {
        f.join(", ")
    }
}

fn display_name(port: &com0com_core::PortName) -> &str {
    if port.is_empty() {
        "(auto)"
    } else {
        port.as_str()
    }
}

pub fn format_pair_row(pair: &Pair) -> String {
    format!(
        "{:<5} {:<11} {:<11} {}",
        pair.id,
        display_name(&pair.params.port_a),
        display_name(&pair.params.port_b),
        describe_flags(&pair.params)
    )
}

pub fn params_block(id: PairId, p: &PairParams) -> String {
    format!(
        "Pair {id}\n  PortA:      {}\n  PortB:      {}\n  Flags:      {}\n  PinWiring:  A={:02x?} B={:02x?}",
        display_name(&p.port_a),
        display_name(&p.port_b),
        describe_flags(p),
        p.pin_wiring_a.lines,
        p.pin_wiring_b.lines
    )
}

pub fn print_help() {
    println!(
        "setupc - configure the Rust com0com virtual null-modem driver

USAGE:
  setupc list
  setupc add   [--id N] [--port-a NAME] [--port-b NAME]
               [--baud-emu] [--overrun] [--noise[=PROB]] [--exclusive] [--plugin]
  setupc remove <id>
  setupc params <id>
  setupc set   <id> [same flags as add, minus --id/--port-*]

PORT NAME:
  Omit --port-a/--port-b to let the COM Name Arbiter assign COMx.
  Pairs created with an explicit id persist that logical pairing id; the bus
  auto-assigns the lowest free id when --id is omitted.

EXAMPLES:
  setupc add --port-a COM15 --port-b COM16
  setupc add --baud-emu --noise=500        (5% noise, auto ids/names)
  setupc list
  setupc params 0
  setupc set 0 --overrun
  setupc remove 0

NOTE:
  Requires com0combus.sys to be loaded and run with Administrator rights.
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use com0com_core::{PinWiring, PortName};

    #[test]
    fn describe_flags_variants() {
        let p = PairParams::default();
        assert_eq!(describe_flags(&p), "default");

        let p = PairParams {
            baud_emu: true,
            ..PairParams::default()
        };
        assert_eq!(describe_flags(&p), "baud-emu");

        // noise_probability is 0.01% units: 3000 -> 30%.
        let p = PairParams {
            noise_enabled: true,
            noise_probability: 3000,
            ..PairParams::default()
        };
        assert_eq!(describe_flags(&p), "noise(30%)");

        // Order: baud-emu, overrun, noise, exclusive, plugin.
        let p = PairParams {
            baud_emu: true,
            overrun: true,
            exclusive: true,
            plugin: true,
            ..PairParams::default()
        };
        assert_eq!(describe_flags(&p), "baud-emu, overrun, exclusive, plugin");
    }

    #[test]
    fn pair_row_matches_the_legacy_format() {
        let pair = Pair {
            id: 7,
            params: PairParams {
                port_a: PortName::new("COM15").unwrap(),
                port_b: PortName::empty(),
                baud_emu: true,
                ..PairParams::default()
            },
        };
        assert_eq!(
            format_pair_row(&pair),
            "7     COM15       (auto)      baud-emu"
        );
    }

    #[test]
    fn params_block_matches_the_legacy_format() {
        let p = PairParams {
            port_a: PortName::new("COM20").unwrap(),
            port_b: PortName::new("COM21").unwrap(),
            noise_enabled: true,
            noise_probability: 500,
            pin_wiring_a: PinWiring::NULL_MODEM,
            pin_wiring_b: PinWiring::NONE,
            ..PairParams::default()
        };
        assert_eq!(
            params_block(5, &p),
            "Pair 5\n  PortA:      COM20\n  PortB:      COM21\n  Flags:      noise(5%)\n  PinWiring:  A=[10, 20, 80, 40] B=[00, 00, 00, 00]"
        );
    }
}
