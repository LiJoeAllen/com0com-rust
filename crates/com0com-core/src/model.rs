//! Domain types shared by every backend.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SdkError;

/// Pair identifier. Valid range is `0..=MAX_PAIR_ID`.
pub type PairId = u8;
/// 0 = port A, 1 = port B.
pub type PortIndex = u8;

pub const MAX_PAIR_ID: PairId = 63;
pub const PAIR_COUNT: usize = 64;
/// Windows allows at most 31 UTF-16 units (+ NUL) in `Com0comPairParams`.
pub const MAX_PORT_NAME_CHARS: usize = 31;
pub const MAX_PAIRS: usize = PAIR_COUNT;
pub const PORT_INDEX_A: PortIndex = 0;
pub const PORT_INDEX_B: PortIndex = 1;

/// A port name (`COM15` on Windows) or a Unix label / link path.
///
/// Empty is meaningful: Windows lets the COM Name Arbiter pick a name and the
/// Unix backend derives `A{id}` / `B{id}`. The 31-character limit only applies
/// to stored pair names (`PairParams::validate`); monitor targets may be full
/// Unix paths. Backends enforce their platform specific character whitelist.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PortName(String);

impl PortName {
    pub fn empty() -> Self {
        Self(String::new())
    }

    pub fn new(name: impl Into<String>) -> Result<Self, SdkError> {
        let name = name.into();
        if name.chars().any(|c| c.is_control()) {
            return Err(SdkError::InvalidArgument(
                "port name must not contain control characters".into(),
            ));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for PortName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for PortName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for PortName {
    type Err = SdkError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for PortName {
    type Error = SdkError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Modem-status constants used by the Windows pin-wiring wire format.
pub const PIN_TO_CTS: u8 = 0x10;
pub const PIN_TO_DSR: u8 = 0x20;
pub const PIN_TO_RI: u8 = 0x40;
pub const PIN_TO_DCD: u8 = 0x80;

/// Per-port output-to-peer-input mapping.
///
/// `lines[0..4]` are the modem-status bits the peer observes for this port's
/// RTS, DTR, OUT1 and OUT2 respectively. The representation matches the
/// Windows `Com0comPairParams.pin_wiring` bytes exactly, so the backend
/// conversion is lossless and `setupc params` keeps printing identical bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinWiring {
    pub lines: [u8; 4],
}

impl PinWiring {
    pub const NONE: Self = Self { lines: [0; 4] };
    pub const NULL_MODEM: Self = Self {
        lines: [PIN_TO_CTS, PIN_TO_DSR, PIN_TO_DCD, PIN_TO_RI],
    };
}

impl Default for PinWiring {
    fn default() -> Self {
        Self::NULL_MODEM
    }
}

/// Per-pair configuration. `set_params` replaces the whole struct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairParams {
    pub port_a: PortName,
    pub port_b: PortName,
    pub baud_emu: bool,
    pub overrun: bool,
    pub noise_enabled: bool,
    /// 0.01% units: `0..=10000`.
    pub noise_probability: u16,
    pub exclusive: bool,
    pub plugin: bool,
    pub pin_wiring_a: PinWiring,
    pub pin_wiring_b: PinWiring,
}

impl Default for PairParams {
    fn default() -> Self {
        Self {
            port_a: PortName::empty(),
            port_b: PortName::empty(),
            baud_emu: false,
            overrun: false,
            noise_enabled: false,
            noise_probability: 0,
            exclusive: false,
            plugin: false,
            pin_wiring_a: PinWiring::NULL_MODEM,
            pin_wiring_b: PinWiring::NULL_MODEM,
        }
    }
}

impl PairParams {
    pub fn validate(&self) -> Result<(), SdkError> {
        if self.noise_probability > 10_000 {
            return Err(SdkError::InvalidArgument(format!(
                "noise_probability {} is outside 0..=10000",
                self.noise_probability
            )));
        }
        for name in [&self.port_a, &self.port_b] {
            if name.as_str().chars().count() > MAX_PORT_NAME_CHARS {
                return Err(SdkError::InvalidArgument(
                    "port name is longer than the platform limit".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub id: PairId,
    pub params: PairParams,
}

/// A port that can be opened by an application.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    /// Full path (Unix) or device name (`COM15`).
    pub name: String,
    pub pair_id: Option<PairId>,
    pub port_index: Option<PortIndex>,
    /// True when this port is created and managed by com0com.
    pub managed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parity {
    None,
    Odd,
    Even,
    Mark,
    Space,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopBits {
    One,
    OnePointFive,
    Two,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowControl {
    None,
    RtsCts,
    XonXoff,
    DtrDsr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortConfig {
    pub baud_rate: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow_control: FlowControl,
    pub read_timeout: Option<std::time::Duration>,
    pub write_timeout: Option<std::time::Duration>,
}

impl Default for PortConfig {
    fn default() -> Self {
        Self {
            baud_rate: 9600,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
            read_timeout: Some(std::time::Duration::from_secs(1)),
            write_timeout: Some(std::time::Duration::from_secs(1)),
        }
    }
}

impl PortConfig {
    pub fn validate(&self) -> Result<(), SdkError> {
        if !(5..=8).contains(&self.data_bits) {
            return Err(SdkError::InvalidArgument(format!(
                "data_bits {} is outside 5..=8",
                self.data_bits
            )));
        }
        if self.baud_rate == 0 {
            return Err(SdkError::InvalidArgument(
                "baud_rate must be non-zero".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_name_limits() {
        assert!(PortName::new("").is_ok());
        assert!(PortName::new("x".repeat(31)).is_ok());
        // Monitor targets may be full Unix link paths longer than a COM name.
        assert!(PortName::new("x".repeat(32)).is_ok());
        assert!(PortName::new("bad\nname").is_err());

        let params = PairParams {
            port_a: PortName::new("x".repeat(32)).unwrap(),
            ..PairParams::default()
        };
        assert!(matches!(
            params.validate(),
            Err(SdkError::InvalidArgument(_))
        ));
    }

    #[test]
    fn pin_wiring_default_is_null_modem() {
        assert_eq!(PinWiring::default(), PinWiring::NULL_MODEM);
        assert_eq!(
            PinWiring::NULL_MODEM.lines,
            [PIN_TO_CTS, PIN_TO_DSR, PIN_TO_DCD, PIN_TO_RI]
        );
        assert_eq!(PairParams::default().pin_wiring_a, PinWiring::NULL_MODEM);
    }

    #[test]
    fn noise_probability_range() {
        let mut params = PairParams {
            noise_probability: 10_000,
            ..PairParams::default()
        };
        assert!(params.validate().is_ok());
        params.noise_probability = 10_001;
        assert!(matches!(
            params.validate(),
            Err(SdkError::InvalidArgument(_))
        ));
    }

    #[test]
    fn port_config_validation() {
        let mut cfg = PortConfig::default();
        assert!(cfg.validate().is_ok());
        cfg.data_bits = 4;
        assert!(cfg.validate().is_err());
        cfg.data_bits = 8;
        cfg.baud_rate = 0;
        assert!(cfg.validate().is_err());
    }
}
