//! Event fan-out and the polling watcher.
//!
//! The kernel driver has no change notifications exposed to user mode, so the
//! watcher periodically diffs `ENUM_PAIRS`. Polling also makes events visible
//! for changes made by another process (for example `setupc`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use com0com_core::{
    DriverKind, DriverStatus, Event, EventFilter, EventSubscription, PairId, PairParams, SdkError,
};

use crate::bus::BusClient;
use crate::transport::BusTransport;

/// Per-subscriber queue capacity. A slow UI drops events (and can read the
/// dropped counter) instead of stalling the watcher.
const EVENT_CHANNEL_CAPACITY: usize = 64;

struct Subscriber {
    filter: EventFilter,
    tx: SyncSender<Event>,
    dropped: Arc<AtomicU64>,
}

#[derive(Default)]
pub(crate) struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

impl EventBus {
    pub(crate) fn subscribe(&self, filter: EventFilter) -> EventSubscription {
        let (tx, rx) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Subscriber {
                filter,
                tx,
                dropped: dropped.clone(),
            });
        EventSubscription::new(id, rx, dropped)
    }

    /// Non-blocking fan-out. Slow subscribers lose events (counted), dead
    /// subscribers are pruned.
    pub(crate) fn publish(&self, event: Event) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|p| p.into_inner());
        subscribers.retain(|subscriber| {
            if !subscriber.filter.matches(&event) {
                return true;
            }
            match subscriber.tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Start the diff watcher once. The thread exits when the backend (the
    /// only strong `EventBus` owner) is dropped.
    ///
    /// `baseline` is the pair snapshot taken when the first subscriber
    /// arrived. Without it the first tick would swallow any change made
    /// between `subscribe` and that tick.
    pub(crate) fn start_watcher(
        self: &Arc<Self>,
        transport: Arc<dyn BusTransport>,
        interval: Duration,
        started: &AtomicBool,
        baseline: Option<BTreeMap<PairId, PairParams>>,
    ) {
        if started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        let spawn = thread::Builder::new()
            .name("com0com-events".into())
            .spawn(move || {
                let bus = BusClient::new(transport);
                let mut previous = baseline;
                let mut driver_running = true;
                loop {
                    thread::sleep(interval);
                    let Some(events) = weak.upgrade() else {
                        break;
                    };
                    match bus.list_pairs() {
                        Ok(pairs) => {
                            if !driver_running {
                                driver_running = true;
                                events.publish(Event::DriverStatusChanged {
                                    driver: DriverKind::Bus,
                                    status: DriverStatus::Running,
                                });
                            }
                            let current: BTreeMap<PairId, PairParams> =
                                pairs.into_iter().map(|p| (p.id, p.params)).collect();
                            if let Some(previous) = previous.replace(current.clone()) {
                                publish_diff(&events, &previous, &current);
                            }
                        }
                        Err(SdkError::DriverUnavailable) => {
                            if driver_running {
                                driver_running = false;
                                previous = None;
                                events.publish(Event::DriverStatusChanged {
                                    driver: DriverKind::Bus,
                                    status: DriverStatus::NotInstalled,
                                });
                            }
                        }
                        // Transient errors keep the last snapshot and retry on
                        // the next tick.
                        Err(_) => {}
                    }
                }
            });
        if spawn.is_err() {
            started.store(false, Ordering::SeqCst);
        }
    }
}

fn publish_diff(
    events: &EventBus,
    previous: &BTreeMap<PairId, PairParams>,
    current: &BTreeMap<PairId, PairParams>,
) {
    for (id, params) in current {
        match previous.get(id) {
            None => events.publish(Event::PairAdded {
                pair: com0com_core::Pair {
                    id: *id,
                    params: params.clone(),
                },
            }),
            Some(old) if old != params => events.publish(Event::ParamsChanged {
                pair: com0com_core::Pair {
                    id: *id,
                    params: params.clone(),
                },
            }),
            Some(_) => {}
        }
    }
    for id in previous.keys() {
        if !current.contains_key(id) {
            events.publish(Event::PairRemoved { id: *id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use com0com_core::{EventKind, Pair};

    fn pair(id: PairId) -> Pair {
        Pair {
            id,
            params: PairParams::default(),
        }
    }

    #[test]
    fn fan_out_respects_filters_and_drops_on_backpressure() {
        let bus = EventBus::default();
        let all = bus.subscribe(EventFilter::all());
        let adds = bus.subscribe(EventFilter::kinds(&[EventKind::PairAdded]));
        for id in 0..(EVENT_CHANNEL_CAPACITY as u8 + 10) {
            bus.publish(Event::PairAdded { pair: pair(id) });
        }
        assert_eq!(all.dropped(), 10);
        assert_eq!(adds.dropped(), 10);
        assert_eq!(all.events().try_iter().count(), EVENT_CHANNEL_CAPACITY);
        assert_eq!(adds.events().try_iter().count(), EVENT_CHANNEL_CAPACITY);

        bus.publish(Event::ParamsChanged { pair: pair(1) });
        assert!(matches!(
            all.try_recv(),
            Ok(Event::ParamsChanged { pair }) if pair.id == 1
        ));
        // The filter-only subscriber never saw ParamsChanged.
        assert!(matches!(
            adds.try_recv(),
            Err(com0com_core::RecvError::Empty)
        ));
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
            .unwrap_or_else(|p| p.into_inner())
            .is_empty());
    }

    #[test]
    fn diff_publishes_add_change_and_remove() {
        let bus = EventBus::default();
        let subscription = bus.subscribe(EventFilter::all());
        let mut previous = BTreeMap::new();
        previous.insert(0u8, PairParams::default());
        let mut current = BTreeMap::new();
        current.insert(
            0u8,
            PairParams {
                baud_emu: true,
                ..PairParams::default()
            },
        );
        current.insert(1u8, PairParams::default());
        publish_diff(&bus, &previous, &current);

        assert!(matches!(
            subscription.try_recv(),
            Ok(Event::ParamsChanged { pair }) if pair.id == 0
        ));
        assert!(matches!(
            subscription.try_recv(),
            Ok(Event::PairAdded { pair }) if pair.id == 1
        ));
        assert!(matches!(
            subscription.try_recv(),
            Err(com0com_core::RecvError::Empty)
        ));

        let older = current.clone();
        let mut newer = BTreeMap::new();
        newer.insert(1u8, PairParams::default());
        publish_diff(&bus, &older, &newer);
        assert!(matches!(
            subscription.try_recv(),
            Ok(Event::PairRemoved { id: 0 })
        ));
    }
}
