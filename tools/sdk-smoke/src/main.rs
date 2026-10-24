//! SDK smoke test. Runs CRUD, events, monitor and state persistence checks
//! against the in-memory backend, so it works on a machine without any driver.

use std::sync::Arc;
use std::time::{Duration, Instant};

use com0com_core::{
    Capabilities, DriverKind, DriverStatus, Event, EventFilter, EventSubscription, MonitorEvent,
    PairParams, PortConfig, PortName, RestoreOptions,
};
use com0com_sdk::{FakeBackend, Monitor, Sdk};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.as_slice() {
        [] => "fake",
        [flag] if flag == "--fake" => "fake",
        [flag] if flag == "--native" => "native",
        [flag, _] if flag == "--daemon" => "daemon",
        _ => {
            eprintln!("usage: sdk-smoke [--fake | --native | --daemon <endpoint>]");
            return std::process::ExitCode::from(2);
        }
    };
    let result = match args.as_slice() {
        [] => run(),
        [flag] if flag == "--fake" => run(),
        [flag] if flag == "--native" => run_native(),
        [flag, endpoint] if flag == "--daemon" => run_daemon(endpoint),
        _ => unreachable!(),
    };
    match result {
        Ok(()) => {
            println!("sdk-smoke: PASS ({mode})");
            std::process::ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("sdk-smoke: FAIL ({mode}): {message}");
            std::process::ExitCode::from(1)
        }
    }
}

/// Real driver (or daemon) run: diagnostics, CRUD, params, events, monitors.
fn run_native() -> Result<(), String> {
    // `--native` must not silently pick up a running daemon on Unix.
    let sdk = Sdk::open_native().map_err(|error| format!("open native backend: {error}"))?;
    exercise_driver(&sdk)
}

fn run_daemon(endpoint: &str) -> Result<(), String> {
    let sdk = Sdk::connect_daemon(Some(endpoint))
        .map_err(|error| format!("connect to {endpoint}: {error}"))?;
    exercise_driver(&sdk)
}

fn exercise_driver(sdk: &Sdk) -> Result<(), String> {
    let capabilities = sdk.capabilities();
    // Diagnostics and event-log access are kernel-driver features. The Unix PTY
    // backend reports them as unsupported, and the smoke test must check the
    // capability bits instead of demanding them from every platform.
    let diagnostics = capabilities.contains(Capabilities::DRIVER_DIAGNOSTICS);
    if diagnostics {
        check_driver(sdk, DriverKind::Bus)?;
    } else {
        println!("sdk-smoke: driver diagnostics unsupported on this backend, skipped");
    }
    if capabilities.contains(Capabilities::DRIVER_LOGS) {
        let logs = sdk
            .driver_logs(16)
            .map_err(|error| format!("driver logs: {error}"))?;
        println!("sdk-smoke: {} driver log entries", logs.len());
    } else {
        println!("sdk-smoke: driver logs unsupported on this backend, skipped");
    }

    let existing = sdk.list_pairs().map_err(|error| error.to_string())?;
    let id = (0u8..=com0com_core::MAX_PAIR_ID)
        .find(|id| !existing.iter().any(|pair| pair.id == *id))
        .ok_or("all pair ids are in use")?;
    sdk.create_pair(Some(id), &PairParams::default())
        .map_err(|error| format!("create pair {id}: {error}"))?;

    let result = (|| {
        let (listed_a, listed_b) = wait_for_listed_ports(sdk, id)?;
        // The port driver is demand-start: after the last pair is removed its
        // service stops, so only check it while a pair exists.
        if diagnostics {
            check_driver(sdk, DriverKind::Port)?;
        }
        exercise_pair(sdk, id, &listed_a, &listed_b)
    })();
    let _ = sdk.delete_pair(id);
    result
}

fn check_driver(sdk: &Sdk, kind: DriverKind) -> Result<(), String> {
    let info = sdk
        .driver_info(kind)
        .map_err(|error| format!("driver info: {error}"))?;
    if info.status != DriverStatus::Running {
        return Err(format!(
            "{} is {:?}, expected running",
            info.service_name, info.status
        ));
    }
    match info.sha256.as_deref() {
        Some(hash) if hash.len() == 64 => {}
        _ => return Err(format!("{} has no SHA-256", info.service_name)),
    }
    println!(
        "sdk-smoke: {} running, version {}, sha256 {}",
        info.service_name,
        info.version.as_deref().unwrap_or("unknown"),
        info.sha256.as_deref().unwrap_or("?")
    );
    Ok(())
}

fn exercise_pair(sdk: &Sdk, id: u8, listed_a: &str, listed_b: &str) -> Result<(), String> {
    // Windows leaves auto-assigned names empty in `get_params` (the effective
    // names live in `list_ports`); the PTY backend stores concrete labels.
    let params = sdk.get_params(id).map_err(|error| error.to_string())?;
    let events = sdk
        .subscribe(EventFilter::all())
        .map_err(|error| format!("subscribe: {error}"))?;

    let mut updated = params.clone();
    updated.baud_emu = !updated.baud_emu;
    sdk.set_params(id, &updated)
        .map_err(|error| format!("set params: {error}"))?;
    wait_for_event(
        &events,
        Duration::from_secs(5),
        |event| matches!(event, Event::ParamsChanged { pair } if pair.id == id && pair.params == updated),
    )?;
    if sdk.get_params(id).map_err(|error| error.to_string())? != updated {
        return Err("params roundtrip mismatch".into());
    }

    // Port A is opened by the published `PortInfo.name` (a full link path on
    // Unix); port B by its pair label when the backend stores one, otherwise
    // by the published name. The daemon must accept both forms.
    let mut monitor_a = open_with_retry(sdk, listed_a)?;
    let b_target = if params.port_b.as_str().is_empty() {
        listed_b
    } else {
        params.port_b.as_str()
    };
    let mut monitor_b = open_with_retry(sdk, b_target)?;
    let forward = vec![0x5a; 1024];
    let backward = vec![0xa5; 1024];
    transfer(&mut monitor_a, &monitor_b, &forward)?;
    transfer(&mut monitor_b, &monitor_a, &backward)?;
    drop(monitor_a);
    drop(monitor_b);

    sdk.delete_pair(id)
        .map_err(|error| format!("delete pair: {error}"))?;
    wait_for_event(
        &events,
        Duration::from_secs(5),
        |event| matches!(event, Event::PairRemoved { id: removed } if *removed == id),
    )?;
    Ok(())
}

fn wait_for_listed_ports(sdk: &Sdk, id: u8) -> Result<(String, String), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let ports = sdk.list_ports().map_err(|error| error.to_string())?;
        let mut names = [None, None];
        for port in ports {
            if !port.managed || port.pair_id != Some(id) {
                continue;
            }
            match port.port_index {
                Some(0) => names[0] = Some(port.name),
                Some(1) => names[1] = Some(port.name),
                _ => {}
            }
        }
        if let [Some(a), Some(b)] = names {
            return Ok((a, b));
        }
        if Instant::now() >= deadline {
            return Err(format!("pair {id} ports missing from list_ports"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_event(
    events: &EventSubscription,
    timeout: Duration,
    mut predicate: impl FnMut(&Event) -> bool,
) -> Result<Event, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("timed out waiting for an event".into());
        }
        match events.recv_timeout(left) {
            Ok(event) if predicate(&event) => return Ok(event),
            Ok(_) => {}
            Err(error) => return Err(format!("event receive: {error:?}")),
        }
    }
}

fn open_with_retry(sdk: &Sdk, port: &str) -> Result<Monitor, String> {
    let name = PortName::new(port).map_err(|error| format!("port {port}: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match sdk.open_monitor(&name, &PortConfig::default()) {
            Ok(monitor) => return Ok(monitor),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!("open {port}: {error}"));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn transfer(writer: &mut Monitor, reader: &Monitor, payload: &[u8]) -> Result<(), String> {
    writer
        .write(payload)
        .map_err(|error| format!("monitor write: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut received = Vec::with_capacity(payload.len());
    while received.len() < payload.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "monitor transfer timed out after {} bytes",
                received.len()
            ));
        }
        match reader.recv_timeout(left) {
            Ok(MonitorEvent::Data(data)) => received.extend_from_slice(&data),
            Ok(MonitorEvent::Error(message)) => return Err(format!("monitor error: {message}")),
            Ok(MonitorEvent::Closed) => return Err("monitor closed during transfer".into()),
            Err(error) => return Err(format!("monitor receive: {error:?}")),
        }
    }
    if received != payload {
        return Err("monitor payload mismatch".into());
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let sdk = Sdk::with_backend(Arc::new(FakeBackend::new()));
    if !sdk.list_pairs().map_err(|e| e.to_string())?.is_empty() {
        return Err("new backend must be empty".into());
    }

    let events = sdk
        .subscribe(EventFilter::all())
        .map_err(|e| e.to_string())?;
    let created = sdk
        .create_pair(None, &PairParams::default())
        .map_err(|e| e.to_string())?;
    if created.id != 0 {
        return Err(format!("expected pair id 0, got {}", created.id));
    }
    match events
        .recv_timeout(Duration::from_secs(1))
        .map_err(|e| format!("{e:?}"))?
    {
        Event::PairAdded { pair } if pair.id == created.id => {}
        other => return Err(format!("unexpected event: {other:?}")),
    }

    let mut params = created.params.clone();
    params.baud_emu = true;
    sdk.set_params(created.id, &params)
        .map_err(|e| e.to_string())?;
    if sdk.get_params(created.id).map_err(|e| e.to_string())? != params {
        return Err("params roundtrip mismatch".into());
    }
    match events
        .recv_timeout(Duration::from_secs(1))
        .map_err(|e| format!("{e:?}"))?
    {
        Event::ParamsChanged { pair } if pair.params == params => {}
        other => return Err(format!("unexpected event: {other:?}")),
    }

    let ports = sdk.list_ports().map_err(|e| e.to_string())?;
    if ports.len() != 2 {
        return Err(format!("expected 2 ports, got {}", ports.len()));
    }
    let a = com0com_core::PortName::new(ports[0].name.clone()).map_err(|e| e.to_string())?;
    let b = com0com_core::PortName::new(ports[1].name.clone()).map_err(|e| e.to_string())?;
    let mut monitor_a = sdk
        .open_monitor(&a, &PortConfig::default())
        .map_err(|e| e.to_string())?;
    let monitor_b = sdk
        .open_monitor(&b, &PortConfig::default())
        .map_err(|e| e.to_string())?;
    monitor_a.write(b"ping").map_err(|e| e.to_string())?;
    match monitor_b
        .recv_timeout(Duration::from_secs(1))
        .map_err(|e| format!("{e:?}"))?
    {
        MonitorEvent::Data(data) if data == b"ping" => {}
        other => return Err(format!("unexpected monitor event: {other:?}")),
    }
    drop(monitor_a);
    drop(monitor_b);

    // State: save, delete, restore from the persisted snapshot.
    let state_dir = std::env::temp_dir().join(format!("com0com-sdk-smoke-{}", std::process::id()));
    let state_path = state_dir.join("state.json");
    let state = sdk.state_at(&state_path);
    let snapshot = state.save().map_err(|e| e.to_string())?;
    if snapshot.pairs.len() != 1 {
        return Err("snapshot must contain the created pair".into());
    }
    if state.load().map_err(|e| e.to_string())? != snapshot {
        return Err("state file roundtrip mismatch".into());
    }
    sdk.delete_pair(created.id).map_err(|e| e.to_string())?;
    let report = state
        .restore(
            &snapshot,
            RestoreOptions {
                dry_run: false,
                remove_extra: false,
                continue_on_error: false,
            },
        )
        .map_err(|e| e.to_string())?;
    if !report.is_success() || !report.applied.contains(&created.id) {
        return Err(format!("restore failed: {report:?}"));
    }
    if sdk.get_params(created.id).map_err(|e| e.to_string())? != params {
        return Err("restored parameters differ".into());
    }

    sdk.delete_pair(created.id).map_err(|e| e.to_string())?;
    if !sdk.list_pairs().map_err(|e| e.to_string())?.is_empty() {
        return Err("pair was not deleted".into());
    }
    match events
        .recv_timeout(Duration::from_secs(1))
        .map_err(|e| format!("{e:?}"))?
    {
        // The delete before the restore and the final delete are both visible.
        Event::PairRemoved { id } if id == created.id => {}
        other => return Err(format!("unexpected event: {other:?}")),
    }
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}
