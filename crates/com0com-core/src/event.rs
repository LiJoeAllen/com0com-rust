//! Events, filters and subscriptions.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{DriverKind, DriverStatus, Pair, PairId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    PairAdded {
        pair: Pair,
    },
    PairRemoved {
        id: PairId,
    },
    ParamsChanged {
        pair: Pair,
    },
    DriverStatusChanged {
        driver: DriverKind,
        status: DriverStatus,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    PairAdded,
    PairRemoved,
    ParamsChanged,
    DriverStatusChanged,
}

impl EventKind {
    const fn bit(self) -> u8 {
        match self {
            Self::PairAdded => 1 << 0,
            Self::PairRemoved => 1 << 1,
            Self::ParamsChanged => 1 << 2,
            Self::DriverStatusChanged => 1 << 3,
        }
    }
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Self::PairAdded { .. } => EventKind::PairAdded,
            Self::PairRemoved { .. } => EventKind::PairRemoved,
            Self::ParamsChanged { .. } => EventKind::ParamsChanged,
            Self::DriverStatusChanged { .. } => EventKind::DriverStatusChanged,
        }
    }

    pub fn pair_id(&self) -> Option<PairId> {
        match self {
            Self::PairAdded { pair } | Self::ParamsChanged { pair } => Some(pair.id),
            Self::PairRemoved { id } => Some(*id),
            Self::DriverStatusChanged { .. } => None,
        }
    }
}

/// Event subscription filter. `all()` matches every event; `for_pair` further
/// restricts pair-scoped events to one pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    kinds: u8,
    pair_id: Option<PairId>,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::all()
    }
}

impl EventFilter {
    pub const fn all() -> Self {
        Self {
            kinds: 0x0f,
            pair_id: None,
        }
    }

    pub const fn none() -> Self {
        Self {
            kinds: 0,
            pair_id: None,
        }
    }

    pub fn kinds(kinds: &[EventKind]) -> Self {
        let bits = kinds.iter().fold(0u8, |acc, kind| acc | kind.bit());
        Self {
            kinds: bits,
            pair_id: None,
        }
    }

    pub const fn for_pair(mut self, id: PairId) -> Self {
        self.pair_id = Some(id);
        self
    }

    pub fn matches(&self, event: &Event) -> bool {
        if self.kinds & event.kind().bit() == 0 {
            return false;
        }
        match self.pair_id {
            None => true,
            Some(id) => event.pair_id() == Some(id),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvError {
    /// No queued item at the moment (non-blocking receive).
    Empty,
    Timeout,
    /// The producer side is gone.
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorEvent {
    Data(Vec<u8>),
    Error(String),
    Closed,
}

/// A live event subscription. Dropping it leaves the backend free to prune the
/// sender on the next failed send.
pub struct EventSubscription {
    id: u64,
    receiver: mpsc::Receiver<Event>,
    dropped: Arc<AtomicU64>,
}

impl EventSubscription {
    pub fn new(id: u64, receiver: mpsc::Receiver<Event>, dropped: Arc<AtomicU64>) -> Self {
        Self {
            id,
            receiver,
            dropped,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn events(&self) -> &mpsc::Receiver<Event> {
        &self.receiver
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(event) => Ok(event),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RecvError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(RecvError::Disconnected),
        }
    }

    pub fn try_recv(&self) -> Result<Event, RecvError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(event),
            Err(mpsc::TryRecvError::Empty) => Err(RecvError::Empty),
            Err(mpsc::TryRecvError::Disconnected) => Err(RecvError::Disconnected),
        }
    }

    /// Number of events discarded because this subscriber was too slow.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PairParams, PinWiring};

    fn pair(id: PairId) -> Pair {
        Pair {
            id,
            params: PairParams::default(),
        }
    }

    #[test]
    fn filter_matches_kinds_and_pair() {
        let added = Event::PairAdded { pair: pair(1) };
        let removed = Event::PairRemoved { id: 1 };
        assert!(EventFilter::all().matches(&added));
        assert!(EventFilter::kinds(&[EventKind::PairAdded]).matches(&added));
        assert!(!EventFilter::kinds(&[EventKind::PairRemoved]).matches(&added));
        assert!(EventFilter::all().for_pair(1).matches(&removed));
        assert!(!EventFilter::all().for_pair(2).matches(&removed));
        assert!(!EventFilter::none().matches(&added));
    }

    #[test]
    fn driver_status_events_have_no_pair() {
        let event = Event::DriverStatusChanged {
            driver: DriverKind::Bus,
            status: DriverStatus::Running,
        };
        assert_eq!(event.pair_id(), None);
        assert!(EventFilter::all().matches(&event));
        assert!(!EventFilter::all().for_pair(0).matches(&event));
    }

    #[test]
    fn pin_wiring_is_part_of_params_equality() {
        let mut a = PairParams::default();
        let mut b = PairParams::default();
        assert_eq!(a, b);
        b.pin_wiring_a = PinWiring::NONE;
        assert_ne!(a, b);
        a.pin_wiring_a = PinWiring::NONE;
        assert_eq!(a, b);
    }
}
