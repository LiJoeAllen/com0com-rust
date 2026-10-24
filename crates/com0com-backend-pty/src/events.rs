//! In-process event fan-out for the PTY backend.
//!
//! The backend has no kernel to notify it of external changes, so the only
//! producer is the backend itself (pair create/delete/set). Subscribers get a
//! bounded queue; a slow subscriber loses events and counts the drops.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use com0com_core::{Event, EventFilter, EventSubscription};

const QUEUE_CAPACITY: usize = 64;

struct Subscriber {
    filter: EventFilter,
    sender: SyncSender<Event>,
    dropped: Arc<AtomicU64>,
}

pub(crate) struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

impl Default for EventBus {
    fn default() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl EventBus {
    pub(crate) fn subscribe(&self, filter: EventFilter) -> EventSubscription {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Subscriber {
                filter,
                sender,
                dropped: Arc::clone(&dropped),
            });
        EventSubscription::new(id, receiver, dropped)
    }

    pub(crate) fn publish(&self, event: Event) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers.retain(|subscriber| {
            if !subscriber.filter.matches(&event) {
                return true;
            }
            match subscriber.sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use com0com_core::{Event, EventKind, Pair, PairParams, MAX_PAIR_ID};

    fn pair(id: u8) -> Pair {
        Pair {
            id,
            params: PairParams::default(),
        }
    }

    #[test]
    fn fan_out_applies_filters_and_counts_drops() {
        let bus = EventBus::default();
        let all = bus.subscribe(EventFilter::all());
        let removed = bus.subscribe(EventFilter::kinds(&[EventKind::PairRemoved]));
        for id in 0..70u8 {
            bus.publish(Event::PairAdded {
                pair: pair(id % MAX_PAIR_ID),
            });
        }
        assert_eq!(all.dropped(), 70 - QUEUE_CAPACITY as u64);
        assert_eq!(removed.dropped(), 0);
        assert_eq!(
            all.events().try_iter().count(),
            QUEUE_CAPACITY,
            "queue must stay bounded"
        );
        assert!(removed.try_recv().is_err());
    }

    #[test]
    fn dropping_a_subscription_prunes_it() {
        let bus = EventBus::default();
        let subscription = bus.subscribe(EventFilter::all());
        drop(subscription);
        bus.publish(Event::PairRemoved { id: 0 });
        assert!(bus
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());
    }
}
