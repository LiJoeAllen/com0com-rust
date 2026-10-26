//! End-to-end daemon tests over the real local transport (named pipe on
//! Windows, Unix socket elsewhere) with the in-memory backend.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use com0com_core::fake::FakeBackend;
use com0com_core::{
    Backend, Capabilities, Event, EventFilter, KernelKind, MonitorEvent, PairParams, PortConfig,
    PortName, SdkError,
};
use com0com_daemon::{Config, RunningDaemon};
use com0com_ipc::RemoteBackend;

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TestCtx {
    dir: PathBuf,
    endpoint: String,
    state: PathBuf,
}

impl TestCtx {
    fn new() -> Self {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(format!("com0comd-it-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (endpoint, state) = if cfg!(windows) {
            (
                format!(r"\\.\pipe\com0comd-it-{unique}"),
                dir.join("state.json"),
            )
        } else {
            (
                dir.join("control.sock").to_string_lossy().into_owned(),
                dir.join("state.json"),
            )
        };
        Self {
            dir,
            endpoint,
            state,
        }
    }

    fn config(&self, restore: bool, autosave: bool) -> Config {
        let mut config = Config::new(&self.endpoint);
        config.state_path = self.state.clone();
        config.restore = restore;
        config.autosave = autosave;
        config.autosave_debounce = Duration::from_millis(50);
        config
    }
}

impl Drop for TestCtx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn spawn(ctx: &TestCtx, restore: bool, autosave: bool) -> RunningDaemon {
    com0com_daemon::spawn(ctx.config(restore, autosave), Arc::new(FakeBackend::new()))
        .expect("daemon starts")
}

fn connect(ctx: &TestCtx) -> RemoteBackend {
    RemoteBackend::connect_with_timeout(Some(&ctx.endpoint), Duration::from_secs(5))
        .expect("client connects")
}

fn params(a: &str, b: &str) -> PairParams {
    PairParams {
        port_a: PortName::new(a).unwrap(),
        port_b: PortName::new(b).unwrap(),
        ..PairParams::default()
    }
}

#[test]
fn crud_events_errors_reconnect_and_shutdown() {
    let ctx = TestCtx::new();
    let config = ctx.config(false, false);
    let daemon = com0com_daemon::spawn(config.clone(), Arc::new(FakeBackend::new())).unwrap();
    assert!(matches!(
        com0com_daemon::spawn(config, Arc::new(FakeBackend::new())),
        Err(SdkError::Busy)
    ));

    let remote = connect(&ctx);
    assert_eq!(
        remote.client().hello().protocol,
        com0com_ipc::PROTOCOL_VERSION
    );
    assert_eq!(remote.client().hello().backend.kernel, KernelKind::None);
    assert!(remote.capabilities().contains(Capabilities::REMOTE_DAEMON));
    assert!(remote.list_pairs().unwrap().is_empty());

    let pair = remote.create_pair(Some(0), &params("A0", "B0")).unwrap();
    assert_eq!(pair.id, 0);
    assert_eq!(remote.list_pairs().unwrap(), vec![pair.clone()]);
    assert_eq!(remote.get_params(0).unwrap().port_b.as_str(), "B0");
    let ports = remote.list_ports().unwrap();
    assert_eq!(ports.len(), 2);
    assert!(ports.iter().all(|port| port.managed));

    let events = remote.subscribe(EventFilter::all()).unwrap();
    let created = remote.create_pair(Some(1), &params("A1", "B1")).unwrap();
    match events.recv_timeout(Duration::from_secs(2)).unwrap() {
        Event::PairAdded { pair } => assert_eq!(pair, created),
        other => panic!("unexpected event: {other:?}"),
    }

    let mut changed = remote.get_params(0).unwrap();
    changed.baud_emu = true;
    remote.set_params(0, &changed).unwrap();
    assert!(remote.get_params(0).unwrap().baud_emu);
    match events.recv_timeout(Duration::from_secs(2)).unwrap() {
        Event::ParamsChanged { pair } => assert_eq!(pair.params, changed),
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(events.dropped(), 0);

    assert!(matches!(remote.get_params(9), Err(SdkError::NotFound)));
    assert!(matches!(
        remote.create_pair(Some(0), &params("C0", "D0")),
        Err(SdkError::AlreadyExists)
    ));
    assert!(matches!(remote.delete_pair(9), Err(SdkError::NotFound)));
    let invalid = PairParams {
        noise_probability: 100_000_001,
        ..PairParams::default()
    };
    assert!(matches!(
        remote.create_pair(Some(3), &invalid),
        Err(SdkError::InvalidArgument(_))
    ));
    assert!(matches!(
        remote.create_pair(Some(64), &params("E", "F")),
        Err(SdkError::InvalidArgument(_))
    ));

    // A dropped connection must not affect the daemon or other sessions.
    let observer = connect(&ctx);
    drop(remote);
    let reconnected = connect(&ctx);
    assert_eq!(reconnected.list_pairs().unwrap().len(), 2);

    daemon.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !observer.client().is_closed() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        observer.client().is_closed(),
        "shutdown closes live client connections"
    );
    assert!(observer.status().is_err());
    assert!(
        RemoteBackend::connect_with_timeout(Some(&ctx.endpoint), Duration::from_millis(300))
            .is_err()
    );
}

#[test]
fn autosave_then_restore_recreates_pairs() {
    let ctx = TestCtx::new();
    let first = spawn(&ctx, false, true);
    {
        let remote = connect(&ctx);
        remote.create_pair(Some(0), &params("A0", "B0")).unwrap();
        let mut five = params("A5", "B5");
        five.baud_emu = true;
        remote.create_pair(Some(5), &five).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let saved = com0com_core::state_io::load(&ctx.state)
                .map(|snapshot| snapshot.pairs.len() == 2)
                .unwrap_or(false);
            if saved {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "autosave never wrote the state file"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    first.join().unwrap();

    let second = spawn(&ctx, true, false);
    let remote = connect(&ctx);
    let pairs = remote.list_pairs().unwrap();
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].id, 0);
    assert_eq!(pairs[1].id, 5);
    assert!(pairs[1].params.baud_emu);
    drop(remote);
    second.join().unwrap();
}

#[test]
fn serial_read_does_not_block_management_requests() {
    let ctx = TestCtx::new();
    let daemon = spawn(&ctx, false, false);
    let serial_client = connect(&ctx);
    serial_client
        .create_pair(Some(0), &params("A0", "B0"))
        .unwrap();
    let management = connect(&ctx);

    let mut a = serial_client
        .open_monitor(&PortName::new("A0").unwrap(), &PortConfig::default())
        .unwrap();
    let b = serial_client
        .open_monitor(&PortName::new("B0").unwrap(), &PortConfig::default())
        .unwrap();

    // With two serial handles open and no data flowing, management calls on a
    // different connection must still complete promptly.
    let started = Instant::now();
    for _ in 0..20 {
        assert_eq!(management.list_pairs().unwrap().len(), 1);
        assert!(management.get_params(0).is_ok());
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "management calls were blocked by serial handles: {:?}",
        started.elapsed()
    );

    assert_eq!(a.write(b"hello").unwrap(), 5);
    match b.recv_timeout(Duration::from_secs(2)).unwrap() {
        MonitorEvent::Data(data) => assert_eq!(data, b"hello"),
        other => panic!("unexpected monitor event: {other:?}"),
    }

    drop(a);
    drop(b);
    drop(management);
    drop(serial_client);
    daemon.join().unwrap();
}
