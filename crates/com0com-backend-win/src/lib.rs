//! Windows backend: SetupAPI + `DeviceIoControl` bridge to the
//! `com0combus`/`com0comport` kernel drivers.
//!
//! The backend is compiled on every host so the workspace stays checkable
//! cross-platform; only [`WinBackend::open`] is Windows-specific. The bus logic
//! itself is exercised on all hosts through [`WinBackend::with_transport`] and
//! an in-memory transport.

mod bus;
mod convert;
#[cfg(windows)]
mod diagnostics;
mod errors;
mod events;
mod monitor;
mod ports;
mod transport;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use com0com_core::{
    Backend, BackendInfo, Capabilities, DriverInfo, DriverKind, EventFilter, EventSubscription,
    KernelKind, LogEntry, MonitorHandle, Pair, PairId, PairParams, PortConfig, PortInfo, PortName,
    SdkError,
};

pub use transport::BusTransport;
#[cfg(windows)]
pub use transport::DeviceTransport;

pub struct WinBackend {
    bus: bus::BusClient,
    events: Arc<events::EventBus>,
    watcher_started: AtomicBool,
    poll_interval: Duration,
}

impl WinBackend {
    /// Backend bound to the real bus control device. The device is opened
    /// lazily so this succeeds even before the driver is loaded; operations
    /// then fail with [`SdkError::DriverUnavailable`].
    pub fn open() -> Result<Self, SdkError> {
        #[cfg(windows)]
        {
            Ok(Self::with_transport(Arc::new(DeviceTransport::new())))
        }
        #[cfg(not(windows))]
        {
            Err(SdkError::Unsupported)
        }
    }

    /// Build a backend over a caller-supplied transport. Tests use this to
    /// exercise the real IOCTL encoding without a driver.
    pub fn with_transport(transport: Arc<dyn BusTransport>) -> Self {
        Self::with_poll_interval(transport, Duration::from_millis(500))
    }

    fn with_poll_interval(transport: Arc<dyn BusTransport>, poll_interval: Duration) -> Self {
        Self {
            bus: bus::BusClient::new(transport),
            events: Arc::new(events::EventBus::default()),
            watcher_started: AtomicBool::new(false),
            poll_interval,
        }
    }
}

impl Backend for WinBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo::new("windows", KernelKind::WindowsKmdfWdm, None)
    }

    fn capabilities(&self) -> Capabilities {
        // Only bits backed by the implementation are advertised.
        Capabilities::PAIR_MANAGEMENT
            | Capabilities::PARAM_BAUD_EMU
            | Capabilities::PARAM_NOISE
            | Capabilities::PARAM_OVERRUN
            | Capabilities::PARAM_EXCLUSIVE
            | Capabilities::PARAM_PLUGIN
            | Capabilities::PARAM_PIN_WIRING
            | Capabilities::SERIAL_MONITOR
            | Capabilities::DRIVER_DIAGNOSTICS
            | Capabilities::DRIVER_LOGS
            | Capabilities::STATE_PERSISTENCE
            | Capabilities::EVENTS
            // 后端可由 com0comd 通过 named pipe 暴露（Windows 侧同样支持）。
            | Capabilities::REMOTE_DAEMON
    }

    fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        self.bus.list_pairs()
    }

    fn create_pair(&self, id: Option<PairId>, params: &PairParams) -> Result<Pair, SdkError> {
        self.bus.create_pair(id, params)
    }

    fn delete_pair(&self, id: PairId) -> Result<(), SdkError> {
        self.bus.delete_pair(id)
    }

    fn get_params(&self, id: PairId) -> Result<PairParams, SdkError> {
        self.bus.get_params(id)
    }

    fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError> {
        self.bus.set_params(id, params)
    }

    fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
        ports::list_ports()
    }

    fn open_monitor(
        &self,
        port: &PortName,
        cfg: &PortConfig,
    ) -> Result<Box<dyn MonitorHandle>, SdkError> {
        monitor::open(port, cfg)
    }

    fn driver_info(&self, kind: DriverKind) -> Result<DriverInfo, SdkError> {
        #[cfg(windows)]
        {
            diagnostics::driver_info(kind)
        }
        #[cfg(not(windows))]
        {
            let _ = kind;
            Err(SdkError::Unsupported)
        }
    }

    fn driver_logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        #[cfg(windows)]
        {
            diagnostics::driver_logs(limit)
        }
        #[cfg(not(windows))]
        {
            let _ = limit;
            Err(SdkError::Unsupported)
        }
    }

    fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError> {
        // Snapshot before the watcher starts so a change made right after
        // `subscribe` is still reported by the first diff tick.
        let baseline: Option<BTreeMap<PairId, PairParams>> =
            self.bus.list_pairs().ok().map(|pairs| {
                pairs
                    .into_iter()
                    .map(|pair| (pair.id, pair.params))
                    .collect()
            });
        self.events.start_watcher(
            self.bus.transport(),
            self.poll_interval,
            &self.watcher_started,
            baseline,
        );
        Ok(self.events.subscribe(filter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;
    use std::sync::Mutex;

    use com0com_common::ioctls;
    use com0com_common::protocol::{
        Com0comPairParams, CreatePairRequest, DeletePairRequest, EnumPairEntry, EnumPairsResponse,
        GetParamsRequest, SetParamsRequest,
    };

    use crate::errors::map_ioctl;
    use crate::transport::{struct_bytes, struct_of};

    #[derive(Default)]
    struct FakeBus {
        pairs: Mutex<Vec<(u32, Com0comPairParams)>>,
    }

    impl BusTransport for FakeBus {
        fn ioctl(
            &self,
            op: &'static str,
            code: u32,
            input: &[u8],
            output: &mut [u8],
        ) -> Result<usize, SdkError> {
            match code {
                c if c == ioctls::IOCTL_COM0COM_ENUM_PAIRS => {
                    let pairs = self.pairs.lock().unwrap();
                    let mut resp = EnumPairsResponse {
                        count: pairs.len() as u32,
                        ..EnumPairsResponse::default()
                    };
                    for (i, (id, params)) in pairs.iter().enumerate() {
                        resp.entries[i] = EnumPairEntry {
                            pair_id: *id,
                            params: *params,
                        };
                    }
                    let bytes = struct_bytes(&resp);
                    output[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
                c if c == ioctls::IOCTL_COM0COM_CREATE_PAIR => {
                    let req: CreatePairRequest = struct_of(input);
                    let mut pairs = self.pairs.lock().unwrap();
                    if pairs.iter().any(|(id, _)| *id == req.pair_id) {
                        return Err(map_ioctl(op, 183));
                    }
                    pairs.push((req.pair_id, req.params));
                    Ok(0)
                }
                c if c == ioctls::IOCTL_COM0COM_DELETE_PAIR => {
                    let req: DeletePairRequest = struct_of(input);
                    let mut pairs = self.pairs.lock().unwrap();
                    let before = pairs.len();
                    pairs.retain(|(id, _)| *id != req.pair_id);
                    if pairs.len() == before {
                        return Err(map_ioctl(op, 87));
                    }
                    Ok(0)
                }
                c if c == ioctls::IOCTL_COM0COM_GET_PARAMS => {
                    let req: GetParamsRequest = struct_of(input);
                    let pairs = self.pairs.lock().unwrap();
                    let Some((_, params)) = pairs.iter().find(|(id, _)| *id == req.pair_id) else {
                        return Err(map_ioctl(op, 87));
                    };
                    let bytes = struct_bytes(params);
                    output[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
                c if c == ioctls::IOCTL_COM0COM_SET_PARAMS => {
                    let req: SetParamsRequest = struct_of(input);
                    let mut pairs = self.pairs.lock().unwrap();
                    let Some((_, params)) = pairs.iter_mut().find(|(id, _)| *id == req.pair_id)
                    else {
                        return Err(map_ioctl(op, 87));
                    };
                    *params = req.params;
                    Ok(0)
                }
                _ => Err(SdkError::Unsupported),
            }
        }
    }

    fn backend() -> WinBackend {
        WinBackend::with_transport(Arc::new(FakeBus::default()))
    }

    fn params(a: &str, b: &str) -> PairParams {
        PairParams {
            port_a: PortName::new(a).unwrap(),
            port_b: PortName::new(b).unwrap(),
            ..PairParams::default()
        }
    }

    #[test]
    fn info_and_capabilities() {
        let backend = backend();
        let info = backend.info();
        assert_eq!(info.platform, "windows");
        assert_eq!(info.kernel, KernelKind::WindowsKmdfWdm);
        assert_eq!(info.protocol_version, com0com_core::PROTOCOL_VERSION);
        let caps = backend.capabilities();
        for capability in [
            Capabilities::PAIR_MANAGEMENT,
            Capabilities::PARAM_BAUD_EMU,
            Capabilities::PARAM_NOISE,
            Capabilities::PARAM_OVERRUN,
            Capabilities::PARAM_EXCLUSIVE,
            Capabilities::PARAM_PLUGIN,
            Capabilities::PARAM_PIN_WIRING,
            Capabilities::SERIAL_MONITOR,
            Capabilities::STATE_PERSISTENCE,
            Capabilities::EVENTS,
            Capabilities::REMOTE_DAEMON,
        ] {
            assert!(caps.contains(capability));
        }
    }

    #[test]
    fn crud_roundtrip() {
        let backend = backend();
        let mut p = params("COM15", "COM16");
        p.baud_emu = true;
        p.noise_enabled = true;
        p.noise_probability = 25_000_000;
        let created = backend.create_pair(None, &p).unwrap();
        assert_eq!(created.id, 0);
        assert_eq!(created.params, p);
        assert_eq!(backend.list_pairs().unwrap(), vec![created]);
        assert_eq!(backend.get_params(0).unwrap(), p);

        let mut updated = p.clone();
        updated.port_a = PortName::new("COM25").unwrap();
        updated.overrun = true;
        backend.set_params(0, &updated).unwrap();
        assert_eq!(backend.get_params(0).unwrap(), updated);

        backend.delete_pair(0).unwrap();
        assert!(backend.list_pairs().unwrap().is_empty());
    }

    #[test]
    fn explicit_and_automatic_ids() {
        let backend = backend();
        assert_eq!(
            backend
                .create_pair(Some(5), &params("COM20", "COM21"))
                .unwrap()
                .id,
            5
        );
        assert_eq!(
            backend
                .create_pair(None, &params("COM30", "COM31"))
                .unwrap()
                .id,
            0
        );
        assert_eq!(
            backend.create_pair(Some(5), &params("X", "Y")),
            Err(SdkError::AlreadyExists)
        );
        backend.delete_pair(0).unwrap();
        assert_eq!(
            backend
                .create_pair(None, &params("COM32", "COM33"))
                .unwrap()
                .id,
            0
        );
        let ids: Vec<u8> = backend
            .list_pairs()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec![5, 0]);
    }

    #[test]
    fn missing_pairs_map_to_not_found() {
        let backend = backend();
        assert_eq!(backend.get_params(3), Err(SdkError::NotFound));
        assert_eq!(backend.delete_pair(3), Err(SdkError::NotFound));
        assert_eq!(
            backend.set_params(3, &PairParams::default()),
            Err(SdkError::NotFound)
        );
    }

    #[test]
    fn invalid_arguments_are_rejected_before_ioctl() {
        let backend = backend();
        assert!(matches!(
            backend.create_pair(Some(64), &PairParams::default()),
            Err(SdkError::InvalidArgument(_))
        ));
        assert!(matches!(
            backend.get_params(64),
            Err(SdkError::InvalidArgument(_))
        ));
        let bad = PairParams {
            noise_probability: 100_000_001,
            ..PairParams::default()
        };
        assert!(matches!(
            backend.create_pair(None, &bad),
            Err(SdkError::InvalidArgument(_))
        ));
        // 16 emoji = 16 chars (accepted by PortName) but 32 UTF-16 units.
        let emoji = PairParams {
            port_a: PortName::new("\u{1F600}".repeat(16)).unwrap(),
            ..PairParams::default()
        };
        assert!(matches!(
            backend.create_pair(None, &emoji),
            Err(SdkError::InvalidArgument(_))
        ));
        assert!(backend.list_pairs().unwrap().is_empty());
    }

    #[test]
    fn watcher_publishes_diffs_from_other_clients() {
        use com0com_core::Event;

        let transport = Arc::new(FakeBus::default());
        let backend = WinBackend::with_poll_interval(
            Arc::clone(&transport) as Arc<dyn BusTransport>,
            Duration::from_millis(20),
        );
        let subscription = backend.subscribe(EventFilter::all()).unwrap();
        // Let the first (baseline) poll complete before mutating anything.
        std::thread::sleep(Duration::from_millis(120));

        backend
            .create_pair(Some(4), &params("COM44", "COM45"))
            .unwrap();
        match subscription.recv_timeout(Duration::from_secs(2)).unwrap() {
            Event::PairAdded { pair } => assert_eq!(pair.id, 4),
            other => panic!("unexpected event: {other:?}"),
        }

        let mut updated = params("COM44", "COM46");
        updated.baud_emu = true;
        backend.set_params(4, &updated).unwrap();
        match subscription.recv_timeout(Duration::from_secs(2)).unwrap() {
            Event::ParamsChanged { pair } => assert_eq!(pair.params, updated),
            other => panic!("unexpected event: {other:?}"),
        }

        backend.delete_pair(4).unwrap();
        assert_eq!(
            subscription.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::PairRemoved { id: 4 }
        );
    }

    #[test]
    fn watcher_reports_change_made_right_after_subscribe() {
        use com0com_core::Event;

        let transport = Arc::new(FakeBus::default());
        let backend = WinBackend::with_poll_interval(
            Arc::clone(&transport) as Arc<dyn BusTransport>,
            Duration::from_millis(20),
        );
        backend
            .create_pair(Some(1), &params("COM31", "COM32"))
            .unwrap();

        let subscription = backend.subscribe(EventFilter::all()).unwrap();
        // No sleep: the change lands between `subscribe` and the first tick.
        let mut updated = params("COM31", "COM32");
        updated.baud_emu = true;
        backend.set_params(1, &updated).unwrap();

        match subscription.recv_timeout(Duration::from_secs(2)).unwrap() {
            Event::ParamsChanged { pair } => {
                assert_eq!(pair.id, 1);
                assert_eq!(pair.params, updated);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn full_auto_id_allocation_returns_busy() {
        let backend = backend();
        for id in 0..com0com_core::MAX_PAIRS as u8 {
            backend
                .create_pair(Some(id), &params("", ""))
                .unwrap_or_else(|e| panic!("id {id}: {e}"));
        }
        assert_eq!(
            backend.create_pair(None, &params("", "")),
            Err(SdkError::Busy)
        );
        assert_eq!(backend.list_pairs().unwrap().len(), com0com_core::MAX_PAIRS);
        // Explicit ids beyond the range never reach the driver.
        assert!(matches!(
            backend.create_pair(Some(200), &params("", "")),
            Err(SdkError::InvalidArgument(_))
        ));
        let _ = size_of::<Com0comPairParams>();
    }
}
