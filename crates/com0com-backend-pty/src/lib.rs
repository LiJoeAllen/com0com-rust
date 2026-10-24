//! Linux/macOS user-mode PTY backend.
//!
//! Each pair owns two PTYs bridged by a pump thread; applications open the
//! stable symlink, not the raw `/dev/pts/N` node. Baud emulation, noise and
//! overrun drop are applied in user space. Capabilities that a PTY cannot
//! provide (exclusive open, plugin, pin wiring, driver diagnostics) stay
//! `false` and the corresponding parameters are rejected with `Unsupported`
//! instead of being silently accepted.

#[cfg(unix)]
mod events;
#[cfg(unix)]
mod links;
#[cfg(unix)]
mod monitor;
#[cfg(unix)]
mod pty;

#[cfg(unix)]
mod imp {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, MutexGuard};

    use com0com_core::{
        Backend, BackendInfo, Capabilities, DriverInfo, DriverKind, DriverStatus, Event,
        EventFilter, EventSubscription, KernelKind, LogEntry, MonitorHandle, Pair, PairId,
        PairParams, PinWiring, PortConfig, PortInfo, PortName, SdkError, MAX_PAIR_ID,
    };

    use crate::events::EventBus;
    use crate::links::{LinkConfig, LinkStyle};
    use crate::monitor;
    use crate::pty::{PtyPair, PtyPort};

    const CAPABILITIES: Capabilities = Capabilities::PAIR_MANAGEMENT
        .union(Capabilities::PARAM_BAUD_EMU)
        .union(Capabilities::PARAM_NOISE)
        .union(Capabilities::PARAM_OVERRUN)
        .union(Capabilities::SERIAL_MONITOR)
        .union(Capabilities::STATE_PERSISTENCE)
        .union(Capabilities::EVENTS)
        .union(Capabilities::REMOTE_DAEMON);

    struct Entry {
        params: PairParams,
        pair: PtyPair,
    }

    pub struct PtyBackend {
        pairs: Arc<Mutex<BTreeMap<PairId, Entry>>>,
        bus: Arc<EventBus>,
        links: LinkConfig,
    }

    impl PtyBackend {
        pub fn open() -> Result<Self, SdkError> {
            Ok(Self::with_links(LinkConfig::resolve()?))
        }

        /// Use an explicit link directory. Used by tests and by deployments
        /// that cannot write the platform default.
        pub fn with_dev_dir(dir: impl Into<PathBuf>) -> Result<Self, SdkError> {
            let style = if cfg!(target_os = "macos") {
                LinkStyle::MacOs
            } else {
                LinkStyle::Linux
            };
            Ok(Self::with_links(LinkConfig::at(dir, style, 0o700)?))
        }

        fn with_links(links: LinkConfig) -> Self {
            Self {
                pairs: Arc::new(Mutex::new(BTreeMap::new())),
                bus: Arc::new(EventBus::default()),
                links,
            }
        }

        fn lock(&self) -> MutexGuard<'_, BTreeMap<PairId, Entry>> {
            self.pairs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        /// Validate a port label and derive the default `A{id}` / `B{id}`.
        fn label(requested: &PortName, id: PairId, index: u8) -> Result<String, SdkError> {
            let label = if requested.is_empty() {
                format!("{}{id}", if index == 0 { 'A' } else { 'B' })
            } else {
                requested.as_str().to_string()
            };
            if label.len() > 31
                || label == "."
                || label == ".."
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err(SdkError::InvalidArgument(format!(
                    "port label {label:?} must be 1..=31 of [A-Za-z0-9._-]"
                )));
            }
            Ok(label)
        }

        /// Parameters the PTY bridge cannot honour are rejected, never faked.
        fn ensure_supported(params: &PairParams) -> Result<(), SdkError> {
            if params.exclusive || params.plugin {
                return Err(SdkError::Unsupported);
            }
            if params.pin_wiring_a != PinWiring::NULL_MODEM
                || params.pin_wiring_b != PinWiring::NULL_MODEM
            {
                return Err(SdkError::Unsupported);
            }
            Ok(())
        }

        fn labels(&self, params: &PairParams, id: PairId) -> Result<[String; 2], SdkError> {
            Ok([
                Self::label(&params.port_a, id, 0)?,
                Self::label(&params.port_b, id, 1)?,
            ])
        }

        /// Store the effective labels instead of an empty "auto" marker, so
        /// `get_params` and events expose the same names `list_ports` reports.
        fn normalize(
            &self,
            params: &PairParams,
            id: PairId,
        ) -> Result<(PairParams, [String; 2]), SdkError> {
            let labels = self.labels(params, id)?;
            let mut normalized = params.clone();
            normalized.port_a = PortName::new(labels[0].clone())?;
            normalized.port_b = PortName::new(labels[1].clone())?;
            Ok((normalized, labels))
        }

        fn label_taken(
            inner: &BTreeMap<PairId, Entry>,
            labels: &[String; 2],
            except: PairId,
        ) -> bool {
            if labels[0] == labels[1] {
                return true;
            }
            inner.iter().any(|(&id, entry)| {
                id != except
                    && (entry.pair.a.label == labels[0]
                        || entry.pair.b.label == labels[0]
                        || entry.pair.a.label == labels[1]
                        || entry.pair.b.label == labels[1])
            })
        }
    }

    impl Backend for PtyBackend {
        fn info(&self) -> BackendInfo {
            BackendInfo::new(
                std::env::consts::OS,
                KernelKind::UnixPty,
                Some(env!("CARGO_PKG_VERSION").into()),
            )
        }

        fn capabilities(&self) -> Capabilities {
            CAPABILITIES
        }

        fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
            Ok(self
                .lock()
                .iter()
                .map(|(&id, entry)| Pair {
                    id,
                    params: entry.params.clone(),
                })
                .collect())
        }

        fn create_pair(&self, id: Option<PairId>, params: &PairParams) -> Result<Pair, SdkError> {
            params.validate()?;
            Self::ensure_supported(params)?;
            let mut inner = self.lock();
            let id = match id {
                Some(id) => {
                    if id > MAX_PAIR_ID {
                        return Err(SdkError::InvalidArgument(format!(
                            "pair id {id} is outside 0..={MAX_PAIR_ID}"
                        )));
                    }
                    if inner.contains_key(&id) {
                        return Err(SdkError::AlreadyExists);
                    }
                    id
                }
                None => (0..=MAX_PAIR_ID)
                    .find(|candidate| !inner.contains_key(candidate))
                    .ok_or_else(|| {
                        SdkError::InvalidArgument("no free pair id (limit 64)".into())
                    })?,
            };
            let (normalized, labels) = self.normalize(params, id)?;
            if Self::label_taken(&inner, &labels, id) {
                return Err(SdkError::AlreadyExists);
            }
            let pair = PtyPair::create(id, labels, &normalized, &self.links)?;
            inner.insert(
                id,
                Entry {
                    params: normalized.clone(),
                    pair,
                },
            );
            drop(inner);

            let created = Pair {
                id,
                params: normalized,
            };
            self.bus.publish(Event::PairAdded {
                pair: created.clone(),
            });
            Ok(created)
        }

        fn delete_pair(&self, id: PairId) -> Result<(), SdkError> {
            let mut inner = self.lock();
            if inner.remove(&id).is_none() {
                return Err(SdkError::NotFound);
            }
            drop(inner);
            self.bus.publish(Event::PairRemoved { id });
            Ok(())
        }

        fn get_params(&self, id: PairId) -> Result<PairParams, SdkError> {
            self.lock()
                .get(&id)
                .map(|entry| entry.params.clone())
                .ok_or(SdkError::NotFound)
        }

        fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError> {
            params.validate()?;
            Self::ensure_supported(params)?;
            let (normalized, labels) = self.normalize(params, id)?;
            let mut inner = self.lock();
            if Self::label_taken(&inner, &labels, id) {
                return Err(SdkError::AlreadyExists);
            }
            let entry = inner.get_mut(&id).ok_or(SdkError::NotFound)?;
            entry.pair.relabel(labels, &self.links)?;
            entry.params = normalized.clone();
            entry.pair.set_shared_params(&normalized);
            drop(inner);

            self.bus.publish(Event::ParamsChanged {
                pair: Pair {
                    id,
                    params: normalized,
                },
            });
            Ok(())
        }

        fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
            let inner = self.lock();
            let mut ports = Vec::with_capacity(inner.len() * 2);
            for (&id, entry) in inner.iter() {
                for (index, port) in [(0u8, &entry.pair.a), (1u8, &entry.pair.b)] {
                    ports.push(port_info(id, index, port));
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
            // Only ports owned by this backend are openable: the path is taken
            // from the entry, never from the caller.
            let target = {
                let inner = self.lock();
                let name = port.as_str();
                let candidate = inner
                    .values()
                    .flat_map(|entry| [&entry.pair.a, &entry.pair.b])
                    .find(|candidate| {
                        candidate.label == name || candidate.link.to_string_lossy() == name
                    })
                    .map(|candidate| candidate.target.clone())
                    .ok_or(SdkError::NotFound)?;
                candidate
            };
            Ok(Box::new(monitor::open(&target, cfg)?))
        }

        fn driver_info(&self, kind: DriverKind) -> Result<DriverInfo, SdkError> {
            Ok(DriverInfo {
                kind,
                status: DriverStatus::Running,
                service_name: "com0com-pty".into(),
                image_path: None,
                version: Some(env!("CARGO_PKG_VERSION").into()),
                sha256: None,
            })
        }

        fn driver_logs(&self, _limit: usize) -> Result<Vec<LogEntry>, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError> {
            Ok(self.bus.subscribe(filter))
        }
    }

    fn port_info(id: PairId, index: u8, port: &PtyPort) -> PortInfo {
        PortInfo {
            name: port.link.to_string_lossy().into_owned(),
            pair_id: Some(id),
            port_index: Some(index),
            managed: true,
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use com0com_core::{
        Backend, BackendInfo, Capabilities, DriverInfo, DriverKind, EventFilter, EventSubscription,
        KernelKind, LogEntry, MonitorHandle, Pair, PairId, PairParams, PortConfig, PortInfo,
        PortName, SdkError,
    };

    pub struct PtyBackend {
        _private: (),
    }

    impl PtyBackend {
        pub fn open() -> Result<Self, SdkError> {
            Err(SdkError::Unsupported)
        }
    }

    impl Backend for PtyBackend {
        fn info(&self) -> BackendInfo {
            BackendInfo::new(std::env::consts::OS, KernelKind::UnixPty, None)
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::NONE
        }

        fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn create_pair(&self, _id: Option<PairId>, _params: &PairParams) -> Result<Pair, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn delete_pair(&self, _id: PairId) -> Result<(), SdkError> {
            Err(SdkError::Unsupported)
        }

        fn get_params(&self, _id: PairId) -> Result<PairParams, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn set_params(&self, _id: PairId, _params: &PairParams) -> Result<(), SdkError> {
            Err(SdkError::Unsupported)
        }

        fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn open_monitor(
            &self,
            _port: &PortName,
            _cfg: &PortConfig,
        ) -> Result<Box<dyn MonitorHandle>, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn driver_info(&self, _kind: DriverKind) -> Result<DriverInfo, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn driver_logs(&self, _limit: usize) -> Result<Vec<LogEntry>, SdkError> {
            Err(SdkError::Unsupported)
        }

        fn subscribe(&self, _filter: EventFilter) -> Result<EventSubscription, SdkError> {
            Err(SdkError::Unsupported)
        }
    }
}

pub use imp::PtyBackend;

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use com0com_core::{
        Backend, Event, EventFilter, MonitorEvent, PairParams, PortConfig, PortName, SdkError,
    };

    use super::PtyBackend;
    use crate::links::{LinkConfig, LinkStyle};

    static NEXT: AtomicU32 = AtomicU32::new(1);

    fn link_path(dir: &std::path::Path, label: &str) -> std::path::PathBuf {
        let style = if cfg!(target_os = "macos") {
            LinkStyle::MacOs
        } else {
            LinkStyle::Linux
        };
        LinkConfig::at(dir, style, 0o700).unwrap().link_path(label)
    }

    fn backend() -> (PtyBackend, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "com0com-backend-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let backend = PtyBackend::with_dev_dir(&dir).unwrap();
        (backend, dir)
    }

    fn params(a: &str, b: &str) -> PairParams {
        PairParams {
            port_a: PortName::new(a).unwrap(),
            port_b: PortName::new(b).unwrap(),
            ..PairParams::default()
        }
    }

    #[test]
    fn crud_events_and_default_labels() {
        let (backend, dir) = backend();
        let subscription = backend.subscribe(EventFilter::all()).unwrap();
        let created = backend.create_pair(None, &PairParams::default()).unwrap();
        assert_eq!(created.id, 0);
        assert_eq!(created.params.port_a.as_str(), "A0");
        assert_eq!(created.params.port_b.as_str(), "B0");
        match subscription.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::PairAdded { pair } => assert_eq!(pair, created),
            other => panic!("unexpected event: {other:?}"),
        }
        let ports = backend.list_ports().unwrap();
        assert_eq!(ports.len(), 2);
        assert!(ports.iter().all(|port| port.managed));
        assert!(ports.iter().any(|port| port.name.ends_with("A0")));
        assert!(ports.iter().any(|port| port.name.ends_with("B0")));

        let mut updated = created.params.clone();
        updated.baud_emu = true;
        backend.set_params(0, &updated).unwrap();
        assert_eq!(backend.get_params(0).unwrap(), updated);
        assert!(matches!(
            subscription.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::ParamsChanged { .. }
        ));

        backend.delete_pair(0).unwrap();
        assert!(matches!(
            subscription.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::PairRemoved { id: 0 }
        ));
        assert!(backend.list_pairs().unwrap().is_empty());

        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_duplicate_labels_and_unsupported_params() {
        let (backend, dir) = backend();
        backend.create_pair(Some(0), &params("X", "Y")).unwrap();
        assert_eq!(
            backend.create_pair(Some(1), &params("X", "Z")),
            Err(SdkError::AlreadyExists)
        );
        assert!(matches!(
            backend.create_pair(Some(2), &params("../evil", "Z")),
            Err(SdkError::InvalidArgument(_))
        ));
        let exclusive = PairParams {
            exclusive: true,
            ..PairParams::default()
        };
        assert_eq!(
            backend.create_pair(Some(3), &exclusive),
            Err(SdkError::Unsupported)
        );
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relabel_failure_rolls_back_without_touching_regular_files() {
        let (backend, dir) = backend();
        let created = backend.create_pair(None, &params("RA", "RB")).unwrap();
        let occupied = link_path(&dir, "OCCUPIED");
        std::fs::write(&occupied, b"keep me").unwrap();

        let mut updated = created.params.clone();
        updated.port_a = PortName::new("OCCUPIED").unwrap();
        updated.port_b = PortName::new("R2B").unwrap();
        assert_eq!(
            backend.set_params(created.id, &updated),
            Err(SdkError::AlreadyExists)
        );
        // 回滚只能删除指向本端口的符号链接，绝不能删掉挡路的普通文件。
        assert_eq!(std::fs::read(&occupied).unwrap(), b"keep me");
        assert_eq!(backend.get_params(created.id).unwrap(), created.params);
        for label in ["RA", "RB"] {
            let link = link_path(&dir, label);
            let metadata = std::fs::symlink_metadata(&link).unwrap();
            assert!(
                metadata.file_type().is_symlink(),
                "{label} was not restored"
            );
        }
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn monitor_round_trips_through_the_pump() {
        let (backend, dir) = backend();
        let pair = backend.create_pair(None, &params("MA", "MB")).unwrap();
        let mut a = backend
            .open_monitor(&pair.params.port_a, &PortConfig::default())
            .unwrap();
        let b = backend
            .open_monitor(&pair.params.port_b, &PortConfig::default())
            .unwrap();

        assert_eq!(a.write(b"ping").unwrap(), 4);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut received = Vec::new();
        while received.len() < 4 && std::time::Instant::now() < deadline {
            match b.recv_timeout(Duration::from_millis(200)) {
                Ok(MonitorEvent::Data(data)) => received.extend_from_slice(&data),
                Ok(_) => break,
                Err(com0com_core::RecvError::Timeout) => {}
                Err(_) => break,
            }
        }
        assert_eq!(received, b"ping");

        let unknown = PortName::new("nope").unwrap();
        assert!(matches!(
            backend.open_monitor(&unknown, &PortConfig::default()),
            Err(SdkError::NotFound)
        ));

        drop(a);
        drop(b);
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
