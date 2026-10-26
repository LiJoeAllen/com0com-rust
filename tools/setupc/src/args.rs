//! Command-line argument parsing for setupc.

use com0com_core::{PairParams, PortName, MAX_PORT_NAME_CHARS};

/// 噪声默认值：方便只写 `--noise` 时也真的产生噪声（1%）。
const DEFAULT_NOISE_PROBABILITY: u32 = 1_000_000;

/// 解析噪声概率。支持三种写法：
/// * 百分比：`5%`；
/// * 小数/科学计数法：`0.05`、`1e-4`；
/// * 上游原始单位（1/100_000_000）：纯整数，如 `3000000`。
///
/// 旧版的 `--noise=3000` 表示 30%，现在会被当作 0.003%；迁移时应改写成
/// `--noise=30%`。
fn parse_noise_probability(text: &str) -> Result<u32, String> {
    if let Some(percent) = text.strip_suffix('%') {
        let value = percent
            .trim()
            .parse::<f64>()
            .map_err(|_| "invalid --noise percentage")?;
        return fraction_to_probability(value / 100.0);
    }
    if text.contains('.') || text.contains('e') || text.contains('E') {
        let value = text
            .trim()
            .parse::<f64>()
            .map_err(|_| "invalid --noise fraction")?;
        return fraction_to_probability(value);
    }
    let raw = text
        .trim()
        .parse::<u64>()
        .map_err(|_| "invalid --noise=prob value")?;
    if raw > 100_000_000 {
        return Err("invalid --noise=prob value (0..=100000000)".into());
    }
    Ok(raw as u32)
}

/// 把 `0.0..=1.0` 的概率换算成上游 1/1e8 单位。
fn fraction_to_probability(fraction: f64) -> Result<u32, String> {
    if !fraction.is_finite() || fraction < 0.0 || fraction > 1.0 {
        return Err("noise probability must be within 0..=1 (or 0%..100%)".into());
    }
    Ok((fraction * 100_000_000.0).round() as u32)
}

/// Parse the shared `--id / --port-a / --port-b / --baud-emu / --overrun /
/// --noise[=prob] / --all-data-bits / --exclusive / --plugin` arguments.
/// `--id` is only relevant for `add`; for `set` it is ignored.
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
            "--noise" => {
                pa.params.noise_enabled = true;
                if pa.params.noise_probability == 0 {
                    pa.params.noise_probability = DEFAULT_NOISE_PROBABILITY;
                }
            }
            "--all-data-bits" => pa.params.all_data_bits = true,
            "--exclusive" => pa.params.exclusive = true,
            "--plugin" => pa.params.plugin = true,
            other => {
                if let Some(prob) = other.strip_prefix("--noise=") {
                    pa.params.noise_enabled = true;
                    pa.params.noise_probability = parse_noise_probability(prob)?;
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
            "--noise=30%",
            "--all-data-bits",
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
        assert_eq!(pa.params.noise_probability, 30_000_000);
        assert!(pa.params.all_data_bits);
        assert!(pa.params.exclusive);
        assert!(pa.params.plugin);
        // The parser seeds the default null-modem pin wiring.
        assert_eq!(pa.params.pin_wiring_a, PinWiring::NULL_MODEM);
        assert_eq!(pa.params.pin_wiring_b, PinWiring::NULL_MODEM);
    }

    #[test]
    fn parse_bare_noise_uses_one_percent_default() {
        let pa = parse_pair_args(&args(&["--noise"])).expect("should parse");
        assert!(pa.params.noise_enabled);
        assert_eq!(pa.params.noise_probability, 1_000_000);
    }

    #[test]
    fn parse_noise_accepts_percent_fraction_and_raw_units() {
        let cases = [
            ("--noise=5%", 5_000_000u32),
            ("--noise=0.05", 5_000_000),
            ("--noise=1e-4", 10_000),
            ("--noise=3000", 3000),
            ("--noise=100%", 100_000_000),
            ("--noise=0", 0),
        ];
        for (flag, expected) in cases {
            let pa = parse_pair_args(&args(&[flag])).expect(flag);
            assert!(pa.params.noise_enabled, "{flag}");
            assert_eq!(pa.params.noise_probability, expected, "{flag}");
        }
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
        assert!(parse_pair_args(&args(&["--noise=100000001"])).is_err());
        assert!(parse_pair_args(&args(&["--noise=1.5"])).is_err());
        assert!(parse_pair_args(&args(&["--noise=101%"])).is_err());
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
