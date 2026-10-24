//! Command-line argument parsing for setupc.

use com0com_core::{PairParams, PortName, MAX_PORT_NAME_CHARS};

/// Parse the shared `--id / --port-a / --port-b / --baud-emu / --overrun /
/// --noise[=prob] / --exclusive / --plugin` arguments. `--id` is only relevant
/// for `add`; for `set` it is ignored.
pub struct PairArgs {
    pub id: Option<u32>,
    pub params: PairParams,
}

pub fn parse_pair_args(args: &[String]) -> Result<PairArgs, String> {
    let mut pa = PairArgs {
        id: None,
        params: PairParams::default(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--id" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or("--id requires a value")?
                    .parse::<u32>()
                    .map_err(|_| "invalid --id value")?;
                pa.id = Some(v);
            }
            "--port-a" | "--port-b" => {
                let is_a = a == "--port-a";
                i += 1;
                let value = args.get(i).ok_or("--port-x requires a value")?;
                if value.chars().count() > MAX_PORT_NAME_CHARS {
                    return Err("--port-x name too long (max 31 chars)".into());
                }
                let name = PortName::new(value.clone()).map_err(|e| e.to_string())?;
                if is_a {
                    pa.params.port_a = name;
                } else {
                    pa.params.port_b = name;
                }
            }
            "--baud-emu" => pa.params.baud_emu = true,
            "--overrun" => pa.params.overrun = true,
            "--noise" => pa.params.noise_enabled = true,
            "--exclusive" => pa.params.exclusive = true,
            "--plugin" => pa.params.plugin = true,
            other => {
                // `--noise=3000` style.
                if let Some(prob) = other.strip_prefix("--noise=") {
                    let prob = prob
                        .parse::<u16>()
                        .map_err(|_| "invalid --noise=prob value")?;
                    if prob > 10_000 {
                        return Err("invalid --noise=prob value (0..=10000)".into());
                    }
                    pa.params.noise_enabled = true;
                    pa.params.noise_probability = prob;
                } else {
                    return Err(format!("unknown argument: {other}"));
                }
            }
        }
        i += 1;
    }
    Ok(pa)
}

#[cfg(test)]
mod tests {
    use super::*;
    use com0com_core::PinWiring;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_full_flag_set() {
        let a = args(&[
            "--id",
            "5",
            "--port-a",
            "COM15",
            "--port-b",
            "COM16",
            "--baud-emu",
            "--overrun",
            "--noise=3000",
            "--exclusive",
            "--plugin",
        ]);
        let pa = parse_pair_args(&a).expect("should parse");
        assert_eq!(pa.id, Some(5));
        assert_eq!(pa.params.port_a.as_str(), "COM15");
        assert_eq!(pa.params.port_b.as_str(), "COM16");
        assert!(pa.params.baud_emu);
        assert!(pa.params.overrun);
        assert!(pa.params.noise_enabled);
        assert_eq!(pa.params.noise_probability, 3000);
        assert!(pa.params.exclusive);
        assert!(pa.params.plugin);
        // The parser seeds the default null-modem pin wiring.
        assert_eq!(pa.params.pin_wiring_a, PinWiring::NULL_MODEM);
        assert_eq!(pa.params.pin_wiring_b, PinWiring::NULL_MODEM);
    }

    #[test]
    fn parse_bare_noise_sets_flag_without_probability() {
        let pa = parse_pair_args(&args(&["--noise"])).expect("should parse");
        assert!(pa.params.noise_enabled);
        assert_eq!(pa.params.noise_probability, 0);
    }

    #[test]
    fn parse_defaults_when_no_flags() {
        let pa = parse_pair_args(&[]).expect("should parse");
        assert_eq!(pa.id, None);
        assert!(!pa.params.baud_emu);
        assert_eq!(pa.params.noise_probability, 0);
        assert_eq!(pa.params.pin_wiring_a, PinWiring::NULL_MODEM);
        assert!(pa.params.port_a.is_empty());
        assert!(pa.params.port_b.is_empty());
    }

    #[test]
    fn parse_accepts_31_char_port_name() {
        let name = "C".repeat(31);
        let pa = parse_pair_args(&args(&["--port-a", &name])).expect("should parse");
        assert_eq!(pa.params.port_a.as_str(), name);
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        assert!(parse_pair_args(&args(&["--bogus"])).is_err());
        assert!(parse_pair_args(&args(&["rofl"])).is_err());
    }

    #[test]
    fn parse_rejects_missing_or_bad_values() {
        assert!(parse_pair_args(&args(&["--id"])).is_err());
        assert!(parse_pair_args(&args(&["--id", "notanumber"])).is_err());
        assert!(parse_pair_args(&args(&["--port-a"])).is_err());
        assert!(parse_pair_args(&args(&["--noise=abc"])).is_err());
        assert!(parse_pair_args(&args(&["--noise=10001"])).is_err());
        assert!(parse_pair_args(&args(&["--noise=70000"])).is_err());
    }

    #[test]
    fn parse_rejects_too_long_port_name() {
        let name = "C".repeat(32);
        assert_eq!(
            parse_pair_args(&args(&["--port-a", &name])).err().unwrap(),
            "--port-x name too long (max 31 chars)"
        );
    }
}
