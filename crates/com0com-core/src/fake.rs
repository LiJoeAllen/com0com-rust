//! In-memory [`Backend`] used by SDK/IPC/C ABI tests.
//!
//! It implements the same semantics as the real backends (id allocation,
//! validation, bounded event queues, monitor loopback) without touching a
//! driver or a PTY.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};

use crate::backend::{
    Backend, BackendInfo, DriverInfo, DriverKind, DriverStatus, KernelKind, LogEntry, LogLevel,
    MonitorHandle,
};
use crate::capabilities::Capabilities;
use crate::error::SdkError;
use crate::event::{Event, EventFilter, EventSubscription, MonitorEvent};
use crate::model::{
    Pair, PairId, PairParams, PortConfig, PortInfo, PortName, MAX_PAIR_ID, PORT_INDEX_A,
    PORT_INDEX_B,
};

const EVENT_QUEUE: usize = 64;
const MONITOR_QUEUE: usize = 256;

struct Subscriber {
    filter: EventFilter,
    sender: mpsc::SyncSender<Event>,
    dropped: Arc<AtomicU64>,
}

struct MonitorSlot {
    token: u64,
    sender: mpsc::SyncSender<MonitorEvent>,
}

struct Inner {
    pairs: BTreeMap<PairId, PairParams>,
    subscribers: HashMap<u64, Subscriber>,
    monitors: HashMap<String, MonitorSlot>,
    next_subscription: u64,
    next_monitor: u64,
}

fn port_name(params: &PairParams, id: PairId, index: u8) -> String {
    let requested = if index == PORT_INDEX_A {
        &params.port_a
    } else {
        &params.port_b
    };
    if requested.is_empty() {
        let prefix = if index == PORT_INDEX_A { "A" } else { "B" };
        format!("{prefix}{id}")
    } else {
        requested.as_str().to_string()
    }
}

impl Inner {
    fn emit(&self, event: Event) {
        for subscriber in self.subscribers.values() {
            if !subscriber.filter.matches(&event) {
                continue;
            }
            if subscriber.sender.try_send(event.clone()).is_err() {
                subscriber.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn find_port(&self, name: &str) -> Option<(PairId, u8, String)> {
        for (&id, params) in &self.pairs {
            for index in [PORT_INDEX_A, PORT_INDEX_B] {
                if port_name(params, id, index) == name {
                    let sibling = port_name(params, id, 1 - index);
                    return Some((id, index, sibling));
                }
            }
        }
        None
    }

    fn name_is_taken(&self, name: &str, except: Option<PairId>) -> bool {
        self.pairs.iter().any(|(&id, params)| {
            Some(id) != except
                && [PORT_INDEX_A, PORT_INDEX_B]
                    .iter()
                    .any(|&index| port_name(params, id, index) == name)
        })
    }

    fn validate_names(&self, params: &PairParams, id: PairId) -> Result<(), SdkError> {
        let names = [
            port_name(params, id, PORT_INDEX_A),
            port_name(params, id, PORT_INDEX_B),
        ];
        if names[0] == names[1] {
            return Err(SdkError::AlreadyExists);
        }
        for name in &names {
            if self.name_is_taken(name, Some(id)) {
                return Err(SdkError::AlreadyExists);
            }
        }
        Ok(())
    }
}

pub struct FakeBackend {
    inner: Arc<Mutex<Inner>>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                pairs: BTreeMap::new(),
                subscribers: HashMap::new(),
                monitors: HashMap::new(),
                next_subscription: 1,
                next_monitor: 1,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for FakeBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo::new("fake", KernelKind::None, Some("fake".into()))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        Ok(self
            .lock()
            .pairs
            .iter()
            .map(|(&id, params)| Pair {
                id,
                params: params.clone(),
            })
            .collect())
    }

    fn create_pair(&self, id: Option<PairId>, params: &PairParams) -> Result<Pair, SdkError> {
        params.validate()?;
        let mut inner = self.lock();
        let id = match id {
            Some(id) => {
                if id > MAX_PAIR_ID {
                    return Err(SdkError::InvalidArgument(format!(
                        "pair id {id} is outside 0..={MAX_PAIR_ID}"
                    )));
                }
                if inner.pairs.contains_key(&id) {
                    return Err(SdkError::AlreadyExists);
                }
                id
            }
            None => (0..=MAX_PAIR_ID)
                .find(|candidate| !inner.pairs.contains_key(candidate))
                .ok_or_else(|| SdkError::InvalidArgument("no free pair id (limit 64)".into()))?,
        };
        inner.validate_names(params, id)?;
        inner.pairs.insert(id, params.clone());
        let pair = Pair {
            id,
            params: params.clone(),
        };
        inner.emit(Event::PairAdded { pair: pair.clone() });
        Ok(pair)
    }

    fn delete_pair(&self, id: PairId) -> Result<(), SdkError> {
        let mut inner = self.lock();
        if inner.pairs.remove(&id).is_none() {
            return Err(SdkError::NotFound);
        }
        inner.emit(Event::PairRemoved { id });
        Ok(())
    }

    fn get_params(&self, id: PairId) -> Result<PairParams, SdkError> {
        self.lock()
            .pairs
            .get(&id)
            .cloned()
            .ok_or(SdkError::NotFound)
    }

    fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError> {
        params.validate()?;
        let mut inner = self.lock();
        if !inner.pairs.contains_key(&id) {
            return Err(SdkError::NotFound);
        }
        inner.validate_names(params, id)?;
        inner.pairs.insert(id, params.clone());
        let pair = Pair {
            id,
            params: params.clone(),
        };
        inner.emit(Event::ParamsChanged { pair });
        Ok(())
    }

    fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
        let inner = self.lock();
        let mut ports = Vec::with_capacity(inner.pairs.len() * 2);
        for (&id, params) in &inner.pairs {
            for index in [PORT_INDEX_A, PORT_INDEX_B] {
                ports.push(PortInfo {
                    name: port_name(params, id, index),
                    pair_id: Some(id),
                    port_index: Some(index),
                    managed: true,
                });
            }
        }
        Ok(ports)
    }

    fn open_monitor(
        &self,
        port: &PortName,
        cfg: &PortConfig,
    ) -> Result<Box<dyn MonitorHandle>, SdkError> {
        cfg.validate()?;
        let mut inner = self.lock();
        let name = port.as_str();
        let (_, _, sibling) = inner.find_port(name).ok_or(SdkError::NotFound)?;
        let (sender, receiver) = mpsc::sync_channel(MONITOR_QUEUE);
        let token = inner.next_monitor;
        inner.next_monitor += 1;
        inner
            .monitors
            .insert(name.to_string(), MonitorSlot { token, sender });
        Ok(Box::new(FakeMonitor {
            receiver,
            inner: Arc::clone(&self.inner),
            own_name: name.to_string(),
            sibling,
            token,
            dropped: AtomicU64::new(0),
        }))
    }

    fn driver_info(&self, kind: DriverKind) -> Result<DriverInfo, SdkError> {
        Ok(DriverInfo {
            kind,
            status: DriverStatus::Running,
            service_name: "com0com-fake".into(),
            image_path: None,
            version: Some("0.1.0-fake".into()),
            sha256: None,
        })
    }

    fn driver_logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        let entry = LogEntry {
            timestamp: "1970-01-01T00:00:00Z".into(),
            level: LogLevel::Information,
            message: "fake backend has no driver log".into(),
        };
        Ok(if limit == 0 { Vec::new() } else { vec![entry] })
    }

    fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError> {
        let mut inner = self.lock();
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE);
        let id = inner.next_subscription;
        inner.next_subscription += 1;
        let dropped = Arc::new(AtomicU64::new(0));
        inner.subscribers.insert(
            id,
            Subscriber {
                filter,
                sender,
                dropped: Arc::clone(&dropped),
            },
        );
        Ok(EventSubscription::new(id, receiver, dropped))
    }
}

struct FakeMonitor {
    receiver: mpsc::Receiver<MonitorEvent>,
    inner: Arc<Mutex<Inner>>,
    own_name: String,
    sibling: String,
    token: u64,
    dropped: AtomicU64,
}

impl MonitorHandle for FakeMonitor {
    fn write(&mut self, data: &[u8]) -> Result<usize, SdkError> {
        let sender = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner
                .monitors
                .get(&self.sibling)
                .map(|slot| slot.sender.clone())
        };
        let Some(sender) = sender else {
            // No monitor is open on the peer yet; the fake has no buffer.
            return Ok(0);
        };
        match sender.try_send(MonitorEvent::Data(data.to_vec())) {
            Ok(()) => Ok(data.len()),
            Err(mpsc::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(0)
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(SdkError::Io("peer monitor is closed".into()))
            }
        }
    }

    fn events(&self) -> &mpsc::Receiver<MonitorEvent> {
        &self.receiver
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for FakeMonitor {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner
                .monitors
                .get(&self.own_name)
                .is_some_and(|slot| slot.token == self.token)
            {
                inner.monitors.remove(&self.own_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, RecvError};
    use std::time::Duration;

    fn params(a: &str, b: &str) -> PairParams {
        PairParams {
            port_a: PortName::new(a).unwrap(),
            port_b: PortName::new(b).unwrap(),
            ..PairParams::default()
        }
    }

    #[test]
    fn crud_and_id_allocation() {
        let backend = FakeBackend::new();
        let p0 = backend.create_pair(None, &params("A", "B")).unwrap();
        assert_eq!(p0.id, 0);
        let p5 = backend.create_pair(Some(5), &params("C", "D")).unwrap();
        assert_eq!(p5.id, 5);
        // Explicit duplicate id is rejected.
        assert_eq!(
            backend.create_pair(Some(5), &params("E", "F")),
            Err(SdkError::AlreadyExists)
        );
        // The smallest free id is reused after a delete.
        let p1 = backend.create_pair(Some(1), &params("I", "J")).unwrap();
        assert_eq!(p1.id, 1);
        backend.delete_pair(1).unwrap();
        let auto = backend.create_pair(None, &params("G", "H")).unwrap();
        assert_eq!(auto.id, 1);
        assert!(matches!(backend.delete_pair(9), Err(SdkError::NotFound)));
        assert_eq!(backend.list_pairs().unwrap().len(), 3);
    }

    #[test]
    fn rejects_invalid_and_conflicting_params() {
        let backend = FakeBackend::new();
        backend.create_pair(None, &params("A", "B")).unwrap();
        assert_eq!(
            backend.create_pair(None, &params("A", "C")),
            Err(SdkError::AlreadyExists)
        );
        assert!(matches!(
            backend.create_pair(None, &params("X", "X")),
            Err(SdkError::AlreadyExists)
        ));
        let bad = PairParams {
            noise_probability: 100_000_001,
            ..PairParams::default()
        };
        assert!(matches!(
            backend.create_pair(None, &bad),
            Err(SdkError::InvalidArgument(_))
        ));
        assert!(matches!(
            backend.create_pair(Some(64), &params("Y", "Z")),
            Err(SdkError::InvalidArgument(_))
        ));
    }

    #[test]
    fn params_roundtrip_and_events() {
        let backend = FakeBackend::new();
        let subscription = backend.subscribe(EventFilter::all()).unwrap();
        let created = backend.create_pair(None, &params("A", "B")).unwrap();
        match subscription.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::PairAdded { pair } => assert_eq!(pair, created),
            other => panic!("unexpected event: {other:?}"),
        }

        let mut updated = created.params.clone();
        updated.baud_emu = true;
        backend.set_params(created.id, &updated).unwrap();
        assert_eq!(backend.get_params(created.id).unwrap(), updated);
        match subscription.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::ParamsChanged { pair } => assert_eq!(pair.params, updated),
            other => panic!("unexpected event: {other:?}"),
        }

        backend.delete_pair(created.id).unwrap();
        assert_eq!(
            subscription.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::PairRemoved { id: created.id }
        );
    }

    #[test]
    fn filter_is_applied_per_subscription() {
        let backend = FakeBackend::new();
        let subscription = backend
            .subscribe(EventFilter::kinds(&[EventKind::ParamsChanged]).for_pair(3))
            .unwrap();
        backend.create_pair(Some(0), &params("A", "B")).unwrap();
        assert_eq!(
            subscription.recv_timeout(Duration::from_millis(20)),
            Err(RecvError::Timeout)
        );
        backend.set_params(0, &params("A", "B2")).unwrap();
        // Pair 0 does not match `for_pair(3)`.
        assert_eq!(
            subscription.recv_timeout(Duration::from_millis(20)),
            Err(RecvError::Timeout)
        );
        assert_eq!(subscription.dropped(), 0);
    }

    #[test]
    fn slow_subscriber_drops_are_counted() {
        let backend = FakeBackend::new();
        let subscription = backend.subscribe(EventFilter::all()).unwrap();
        for _ in 0..(EVENT_QUEUE * 2) {
            let pair = backend.create_pair(None, &PairParams::default()).unwrap();
            backend.delete_pair(pair.id).unwrap();
        }
        assert!(subscription.dropped() > 0);
    }

    #[test]
    fn monitor_loopback_between_ports() {
        let backend = FakeBackend::new();
        let pair = backend.create_pair(None, &params("MONA", "MONB")).unwrap();
        let mut a = backend
            .open_monitor(&pair.params.port_a, &PortConfig::default())
            .unwrap();
        let mut b = backend
            .open_monitor(&pair.params.port_b, &PortConfig::default())
            .unwrap();

        assert_eq!(a.write(b"hello").unwrap(), 5);
        match b.recv_timeout(Duration::from_secs(1)).unwrap() {
            MonitorEvent::Data(data) => assert_eq!(data, b"hello"),
            other => panic!("unexpected monitor event: {other:?}"),
        }

        assert_eq!(b.write(b"world").unwrap(), 5);
        match a.recv_timeout(Duration::from_secs(1)).unwrap() {
            MonitorEvent::Data(data) => assert_eq!(data, b"world"),
            other => panic!("unexpected monitor event: {other:?}"),
        }

        let ports = backend.list_ports().unwrap();
        assert_eq!(ports.len(), 2);
        assert!(ports.iter().all(|port| port.managed));
    }

    #[test]
    fn monitor_rejects_unknown_port() {
        let backend = FakeBackend::new();
        let unknown = PortName::new("NOPE").unwrap();
        assert!(matches!(
            backend.open_monitor(&unknown, &PortConfig::default()),
            Err(SdkError::NotFound)
        ));
        let bad_cfg = PortConfig {
            data_bits: 3,
            ..PortConfig::default()
        };
        assert!(matches!(
            backend.open_monitor(&unknown, &bad_cfg),
            Err(SdkError::InvalidArgument(_))
        ));
    }
}
