//! Capability bits exposed to UI code.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Bit flags describing what a backend can actually do.
///
/// UI code must consult these flags before offering a feature. A capability
/// that is not implemented must be `false`; never emulate an unsupported
/// operation silently.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(u32);

impl Capabilities {
    pub const NONE: Self = Self(0);
    pub const PAIR_MANAGEMENT: Self = Self(1 << 0);
    pub const PARAM_BAUD_EMU: Self = Self(1 << 1);
    pub const PARAM_NOISE: Self = Self(1 << 2);
    pub const PARAM_OVERRUN: Self = Self(1 << 3);
    pub const PARAM_EXCLUSIVE: Self = Self(1 << 4);
    pub const PARAM_PLUGIN: Self = Self(1 << 5);
    pub const PARAM_PIN_WIRING: Self = Self(1 << 6);
    pub const SERIAL_MONITOR: Self = Self(1 << 7);
    pub const DRIVER_DIAGNOSTICS: Self = Self(1 << 8);
    pub const DRIVER_LOGS: Self = Self(1 << 9);
    pub const STATE_PERSISTENCE: Self = Self(1 << 10);
    pub const EVENTS: Self = Self(1 << 11);
    pub const REMOTE_DAEMON: Self = Self(1 << 12);

    pub const ALL: Self = Self((1 << 13) - 1);

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = (&'static str, Self)> {
        const BITS: [(&str, Capabilities); 13] = [
            ("PAIR_MANAGEMENT", Capabilities::PAIR_MANAGEMENT),
            ("PARAM_BAUD_EMU", Capabilities::PARAM_BAUD_EMU),
            ("PARAM_NOISE", Capabilities::PARAM_NOISE),
            ("PARAM_OVERRUN", Capabilities::PARAM_OVERRUN),
            ("PARAM_EXCLUSIVE", Capabilities::PARAM_EXCLUSIVE),
            ("PARAM_PLUGIN", Capabilities::PARAM_PLUGIN),
            ("PARAM_PIN_WIRING", Capabilities::PARAM_PIN_WIRING),
            ("SERIAL_MONITOR", Capabilities::SERIAL_MONITOR),
            ("DRIVER_DIAGNOSTICS", Capabilities::DRIVER_DIAGNOSTICS),
            ("DRIVER_LOGS", Capabilities::DRIVER_LOGS),
            ("STATE_PERSISTENCE", Capabilities::STATE_PERSISTENCE),
            ("EVENTS", Capabilities::EVENTS),
            ("REMOTE_DAEMON", Capabilities::REMOTE_DAEMON),
        ];
        BITS.into_iter().filter(move |(_, bit)| self.contains(*bit))
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for Capabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("Capabilities(NONE)");
        }
        f.write_str("Capabilities(")?;
        let mut first = true;
        for (name, _) in self.iter() {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str(name)?;
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_per_bit() {
        let caps = Capabilities::PAIR_MANAGEMENT | Capabilities::EVENTS;
        assert!(caps.contains(Capabilities::PAIR_MANAGEMENT));
        assert!(caps.contains(Capabilities::EVENTS));
        assert!(!caps.contains(Capabilities::DRIVER_LOGS));
        assert!(caps.contains(caps));
        assert!(Capabilities::ALL.contains(caps));
    }

    #[test]
    fn empty_and_all() {
        assert!(Capabilities::NONE.is_empty());
        assert!(!Capabilities::ALL.is_empty());
        assert_eq!(Capabilities::from_bits(u32::MAX), Capabilities::ALL);
        assert_eq!(Capabilities::ALL.iter().count(), 13);
    }
}
