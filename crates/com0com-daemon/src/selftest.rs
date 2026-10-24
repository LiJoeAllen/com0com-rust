//! In-process end-to-end check: real named pipe / Unix socket, fake backend.
//!
//! `com0comd selftest` runs this without a driver so installs and CI can verify
//! the transport, framing, ACL and dispatch layers before touching hardware.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use com0com_core::fake::FakeBackend;
use com0com_core::{
    Backend, Capabilities, Event, EventFilter, KernelKind, MonitorEvent, PairParams, PortConfig,
    PortName, SdkError,
};
use com0com_ipc::RemoteBackend;

use crate::Config;

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Check {
    count: usize,
}

impl Check {
    fn that(&mut self, condition: bool, message: &str) -> Result<(), SdkError> {
        if condition {
            self.count += 1;
            Ok(())
        } else {
            Err(SdkError::Internal(format!("selftest: {message}")))
        }
    }
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32;
    format!(
        "{:x}{:x}{nanos:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("c0c-st-{}", unique()))
}

// macOS stores Unix sockets in `sun_path[104]`, so keep the whole endpoint
// short: long temp directories plus a descriptive prefix overflow the limit.
fn endpoint(dir: &Path) -> (String, PathBuf) {
    let tag = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("c0c-st");
    if cfg!(windows) {
        (format!(r"\\.\pipe\{tag}"), dir.join("state.json"))
    } else {
        (
            dir.join("ctl.sock").to_string_lossy().into_owned(),
            dir.join("state.json"),
        )
    }
}

fn params(a: &str, b: &str) -> PairParams {
    PairParams {
        port_a: PortName::new(a).unwrap(),
        port_b: PortName::new(b).unwrap(),
        ..PairParams::default()
    }
}

/// Returns the number of checks performed.
pub fn selftest() -> Result<usize, SdkError> {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).map_err(|error| SdkError::Io(error.to_string()))?;
    let (endpoint, state_path) = endpoint(&dir);

    let result = run(&endpoint, &state_path);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run(endpoint: &str, state_path: &Path) -> Result<usize, SdkError> {
    let mut check = Check { count: 0 };

    let mut config = Config::new(endpoint);
    config.state_path = state_path.to_path_buf();
    config.autosave = true;
    config.autosave_debounce = Duration::from_millis(50);

    let daemon = crate::spawn(config.clone(), Arc::new(FakeBackend::new()))?;
    check.that(
        daemon.endpoint() == endpoint,
        "listener reports the requested endpoint",
    )?;

    // A second daemon on the same endpoint must be rejected by the singleton.
    match crate::spawn(config.clone(), Arc::new(FakeBackend::new())) {
        Err(SdkError::Busy) => check.that(true, "singleton rejects a second daemon")?,
        Ok(second) => {
            let _ = second.join();
            return Err(SdkError::Internal(
                "selftest: a second daemon started on the same endpoint".into(),
            ));
        }
        Err(other) => {
            return Err(SdkError::Internal(format!(
                "selftest: unexpected second-daemon error: {other}"
            )))
        }
    }

    let remote = RemoteBackend::connect_with_timeout(Some(endpoint), Duration::from_secs(5))?;
    check.that(
        remote.client().hello().protocol == com0com_ipc::PROTOCOL_VERSION,
        "handshake negotiates the current protocol",
    )?;
    check.that(
        remote.client().hello().backend.kernel == KernelKind::None,
        "hello reports the fake backend",
    )?;
    check.that(
        remote
            .capabilities()
            .contains(Capabilities::REMOTE_DAEMON | Capabilities::PAIR_MANAGEMENT),
        "remote capabilities include REMOTE_DAEMON",
    )?;
    check.that(remote.list_pairs()?.is_empty(), "fresh daemon has no pairs")?;

    let pair = remote.create_pair(Some(0), &params("A0", "B0"))?;
    check.that(pair.id == 0, "create allocates the requested id")?;
    check.that(remote.list_pairs()?.len() == 1, "pairs.list sees the pair")?;
    check.that(
        remote.get_params(0)?.port_b.as_str() == "B0",
        "pairs.get returns the stored params",
    )?;

    let mut updated = remote.get_params(0)?;
    updated.baud_emu = true;
    remote.set_params(0, &updated)?;
    check.that(
        remote.get_params(0)?.baud_emu,
        "pairs.set_params persists a change",
    )?;

    let events = remote.subscribe(EventFilter::all())?;
    remote.create_pair(Some(1), &params("A1", "B1"))?;
    match events.recv_timeout(Duration::from_secs(2)) {
        Ok(Event::PairAdded { pair }) => check.that(pair.id == 1, "event carries the new pair")?,
        other => {
            return Err(SdkError::Internal(format!(
                "selftest: unexpected event: {other:?}"
            )))
        }
    }
    drop(events);

    let ports = remote.list_ports()?;
    check.that(
        ports.len() == 4 && ports.iter().all(|port| port.managed),
        "ports.list reports both ports of both pairs",
    )?;

    let mut a = remote.open_monitor(&PortName::new("A0").unwrap(), &PortConfig::default())?;
    let b = remote.open_monitor(&PortName::new("B0").unwrap(), &PortConfig::default())?;
    check.that(
        a.write(b"ping")? == 4,
        "serial.write reports the byte count",
    )?;
    match b.recv_timeout(Duration::from_secs(2)) {
        Ok(MonitorEvent::Data(data)) => {
            check.that(data == b"ping", "serial.data arrives on the peer port")?
        }
        other => {
            return Err(SdkError::Internal(format!(
                "selftest: unexpected serial event: {other:?}"
            )))
        }
    }
    // Management calls stay usable while monitors are open.
    check.that(
        remote.list_pairs()?.len() == 2,
        "management calls work while serial handles are open",
    )?;
    drop(a);
    drop(b);

    check.that(
        matches!(remote.get_params(9), Err(SdkError::NotFound)),
        "unknown pair maps to NotFound",
    )?;
    check.that(
        matches!(
            remote.create_pair(Some(0), &params("C0", "D0")),
            Err(SdkError::AlreadyExists)
        ),
        "duplicate pair id maps to AlreadyExists",
    )?;
    let bad = PairParams {
        noise_probability: 10_001,
        ..PairParams::default()
    };
    check.that(
        matches!(
            remote.create_pair(Some(3), &bad),
            Err(SdkError::InvalidArgument(_))
        ),
        "invalid params map to InvalidArgument",
    )?;

    let saved = remote.save_state()?;
    check.that(
        saved.snapshot.pairs.len() == 2 && Path::new(&saved.path).exists(),
        "state.save writes the daemon state file",
    )?;
    check.that(
        remote.load_state()?.pairs.len() == 2,
        "state.load reads it back",
    )?;

    // Autosave must persist a later mutation without an explicit save.
    remote.create_pair(Some(2), &params("A2", "B2"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut autosaved = false;
    while Instant::now() < deadline {
        if com0com_core::state_io::load(state_path)
            .map(|snapshot| snapshot.pairs.len() == 3)
            .unwrap_or(false)
        {
            autosaved = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    check.that(autosaved, "autosave persists pair mutations")?;

    let status = remote.status()?;
    check.that(
        status.running && status.protocol == com0com_ipc::PROTOCOL_VERSION,
        "daemon.status answers",
    )?;
    check.that(remote.client().ping()?.pong, "rpc.ping answers")?;

    remote.client().shutdown()?;
    drop(remote);
    daemon.join()?;
    check.that(
        RemoteBackend::connect_with_timeout(Some(endpoint), Duration::from_millis(300)).is_err(),
        "endpoint is closed after shutdown",
    )?;

    Ok(check.count)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn unix_endpoint_fits_in_sun_path() {
        let dir = scratch_dir();
        let (endpoint, _) = endpoint(&dir);
        // macOS `sun_path` is 104 bytes including the NUL terminator.
        assert!(
            endpoint.len() <= 100,
            "endpoint is {} bytes: {endpoint}",
            endpoint.len()
        );
    }
}
