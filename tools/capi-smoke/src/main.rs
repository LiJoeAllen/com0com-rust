//! C ABI smoke test.
//!
//! `--self-test` (the default) exercises every export through the in-memory
//! backend, so it runs without a driver or daemon. `--native` repeats the
//! driver-facing subset (diagnostics, CRUD, parameters, events and monitors)
//! against the real kernel driver; the VM guest script uses that mode.
//!
//! Output uses `PASS|` / `FAIL|` lines and a final `RESULT|` line so the VM
//! guest scripts can parse it like the setupc checks.

use std::ffi::{c_char, c_void, CString};
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use com0com_capi::*;

static EVENT_KIND: AtomicU32 = AtomicU32::new(u32::MAX);
static EVENT_PAIR: AtomicU32 = AtomicU32::new(u32::MAX);
static EVENT_COUNT: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn on_event(_user: *mut c_void, event: *const c0c_event) {
    if event.is_null() {
        return;
    }
    EVENT_KIND.store((*event).kind as u32, Ordering::SeqCst);
    EVENT_PAIR.store((*event).pair_id, Ordering::SeqCst);
    EVENT_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[derive(Default)]
struct Smoke {
    checks: u32,
    failures: u32,
}

impl Smoke {
    fn check(&mut self, ok: bool, what: &str) {
        self.checks += 1;
        if ok {
            println!("PASS| {what}");
        } else {
            self.failures += 1;
            println!("FAIL| {what}");
        }
    }
}

fn fill(buf: &mut [c_char], text: &str) {
    let bytes = text.as_bytes();
    let len = bytes.len().min(buf.len().saturating_sub(1));
    for (slot, byte) in buf.iter_mut().zip(bytes.iter()).take(len) {
        *slot = *byte as c_char;
    }
    buf[len] = 0;
}

// c_char is i8 on Windows and u8 on Unix: the cast is required on Windows
// and a no-op on Unix, where clippy would flag it.
#[allow(clippy::unnecessary_cast)]
fn read(buf: &[c_char]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    let bytes: Vec<u8> = buf[..len].iter().map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn params(a: &str, b: &str) -> c0c_params {
    let mut out = c0c_params::default();
    fill(&mut out.port_a, a);
    fill(&mut out.port_b, b);
    out.pin_wiring_a = [0x10, 0x20, 0x80, 0x40];
    out.pin_wiring_b = [0x10, 0x20, 0x80, 0x40];
    out
}

fn open_fake() -> Result<*mut c0c_session, String> {
    open(C0C_BACKEND_FAKE)
}

fn open(backend: i32) -> Result<*mut c0c_session, String> {
    let options = c0c_open_options {
        size: size_of::<c0c_open_options>() as u32,
        backend,
        endpoint: ptr::null(),
    };
    let mut session: *mut c0c_session = ptr::null_mut();
    let code = unsafe { c0c_open(&options, &mut session) };
    if code != C0C_OK || session.is_null() {
        return Err(format!("c0c_open failed: {code}"));
    }
    Ok(session)
}

fn last_error(session: *const c0c_session) -> String {
    let mut buffer = vec![0 as c_char; 256];
    let code =
        unsafe { c0c_session_last_error_message(session, buffer.as_mut_ptr(), buffer.len()) };
    if code == C0C_OK {
        read(&buffer)
    } else {
        format!("error {code}")
    }
}

fn main() -> std::process::ExitCode {
    let native = match std::env::args().nth(1).as_deref() {
        None | Some("--self-test") => false,
        Some("--native") => true,
        Some(_) => {
            eprintln!("usage: capi-smoke [--self-test | --native]");
            return std::process::ExitCode::from(2);
        }
    };
    let mut smoke = Smoke::default();
    let outcome = if native {
        run_native(&mut smoke)
    } else {
        run_fake(&mut smoke)
    };
    if let Err(error) = outcome {
        println!("FAIL| {error}");
        smoke.checks += 1;
        smoke.failures += 1;
    }
    if smoke.failures == 0 {
        println!("RESULT| capi-smoke|{}/{}|PASS", smoke.checks, smoke.checks);
        std::process::ExitCode::SUCCESS
    } else {
        println!(
            "RESULT| capi-smoke|{}/{}|FAIL",
            smoke.checks - smoke.failures,
            smoke.checks
        );
        std::process::ExitCode::from(1)
    }
}

fn run_fake(smoke: &mut Smoke) -> Result<(), String> {
    smoke.check(
        unsafe { c0c_api_version() } == C0C_API_VERSION,
        "api version",
    );

    let session = open_fake()?;
    let mut count = usize::MAX;
    let code = unsafe { c0c_list_pairs(session, ptr::null_mut(), 0, &mut count) };
    smoke.check(code == C0C_OK && count == 0, "empty pair list");

    let first = params("COM11", "COM12");
    let mut info = c0c_pair_info::default();
    let code = unsafe { c0c_create_pair(session, C0C_PAIR_ID_AUTO, &first, &mut info) };
    smoke.check(code == C0C_OK && info.id == 0, "create pair with auto id");

    let second = params("COM21", "COM22");
    let code = unsafe { c0c_create_pair(session, 5, &second, ptr::null_mut()) };
    smoke.check(code == C0C_OK, "create pair id 5");

    let mut one = [c0c_pair_info::default(); 1];
    let mut count = 0;
    let code = unsafe { c0c_list_pairs(session, one.as_mut_ptr(), one.len(), &mut count) };
    smoke.check(
        code == C0C_E_BUFFER_TOO_SMALL && count == 2,
        "short buffer reports the required count",
    );

    let mut two = [c0c_pair_info::default(); 2];
    let mut count = 0;
    let code = unsafe { c0c_list_pairs(session, two.as_mut_ptr(), two.len(), &mut count) };
    smoke.check(
        code == C0C_OK && count == 2 && two[0].id == 0 && two[1].id == 5,
        "full pair list",
    );

    let mut got = c0c_params::default();
    let code = unsafe { c0c_get_params(session, 0, &mut got) };
    smoke.check(
        code == C0C_OK && read(&got.port_a) == "COM11" && read(&got.port_b) == "COM12",
        "get params",
    );

    got.baud_emu = 1;
    got.noise_enabled = 1;
    got.noise_probability = 2_500_000;
    got.all_data_bits = 1;
    let code = unsafe { c0c_set_params(session, 0, &got) };
    let mut updated = c0c_params::default();
    let read_back = unsafe { c0c_get_params(session, 0, &mut updated) };
    smoke.check(
        code == C0C_OK
            && read_back == C0C_OK
            && updated.baud_emu == 1
            && updated.noise_probability == 2_500_000
            && updated.all_data_bits == 1,
        "set params",
    );

    let mut invalid = got;
    invalid.noise_probability = 100_000_001;
    let code = unsafe { c0c_set_params(session, 0, &invalid) };
    smoke.check(
        code == C0C_E_INVALID_ARGUMENT,
        "invalid params map to invalid argument",
    );

    let mut ports = [c0c_port_info::default(); 8];
    let mut count = 0;
    let code = unsafe { c0c_list_ports(session, ports.as_mut_ptr(), ports.len(), &mut count) };
    smoke.check(
        code == C0C_OK && count == 4 && ports[..4].iter().all(|port| port.managed == 1),
        "list managed ports",
    );

    let mut driver = c0c_driver_desc::default();
    let code = unsafe { c0c_driver_info(session, C0C_DRIVER_BUS, &mut driver) };
    smoke.check(
        code == C0C_OK
            && driver.status == C0C_DRIVER_RUNNING
            && read(&driver.service_name) == "com0com-fake",
        "driver info",
    );

    let mut logs = [c0c_log_entry::default(); 2];
    let mut count = 0;
    let code = unsafe { c0c_driver_logs(session, 1, logs.as_mut_ptr(), logs.len(), &mut count) };
    smoke.check(code == C0C_OK && count == 1, "driver logs");

    let code = unsafe { c0c_create_pair(session, 0, &first, ptr::null_mut()) };
    smoke.check(code == C0C_E_ALREADY_EXISTS, "duplicate id is rejected");
    let code = unsafe { c0c_remove_pair(session, 9) };
    smoke.check(code == C0C_E_NOT_FOUND, "missing id is rejected");

    let mut error = c0c_error::default();
    let code = unsafe { c0c_session_last_error(session, &mut error) };
    smoke.check(
        code == C0C_E_NOT_FOUND && read(&error.message) == "pair not found",
        "last error is recorded",
    );
    let mut message = vec![0 as c_char; 256];
    let code =
        unsafe { c0c_session_last_error_message(session, message.as_mut_ptr(), message.len()) };
    smoke.check(
        code == C0C_OK && read(&message) == "pair not found",
        "last error message",
    );

    EVENT_KIND.store(u32::MAX, Ordering::SeqCst);
    EVENT_PAIR.store(u32::MAX, Ordering::SeqCst);
    EVENT_COUNT.store(0, Ordering::SeqCst);
    let mut subscription: *mut c0c_subscription = ptr::null_mut();
    let code =
        unsafe { c0c_subscribe(session, Some(on_event), ptr::null_mut(), &mut subscription) };
    smoke.check(code == C0C_OK && !subscription.is_null(), "subscribe");
    let third = params("COM31", "COM32");
    let code = unsafe { c0c_create_pair(session, 7, &third, ptr::null_mut()) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while EVENT_COUNT.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    smoke.check(
        code == C0C_OK
            && EVENT_KIND.load(Ordering::SeqCst) == C0C_EVENT_PAIR_ADDED as u32
            && EVENT_PAIR.load(Ordering::SeqCst) == 7,
        "event callback",
    );
    let code = unsafe { c0c_unsubscribe(subscription) };
    smoke.check(code == C0C_OK, "unsubscribe");

    let config = c0c_port_config {
        baud_rate: 115_200,
        ..c0c_port_config::default()
    };
    let port_a = CString::new("COM11").unwrap();
    let port_b = CString::new("COM12").unwrap();
    let mut monitor_a: *mut c0c_monitor = ptr::null_mut();
    let mut monitor_b: *mut c0c_monitor = ptr::null_mut();
    let code_a = unsafe { c0c_open_monitor(session, port_a.as_ptr(), &config, &mut monitor_a) };
    let code_b = unsafe { c0c_open_monitor(session, port_b.as_ptr(), &config, &mut monitor_b) };
    smoke.check(
        code_a == C0C_OK && code_b == C0C_OK && !monitor_a.is_null() && !monitor_b.is_null(),
        "open monitors",
    );

    let payload: Vec<u8> = (0..4096u32).map(|index| (index % 251) as u8).collect();
    let echoed = transfer(monitor_a, monitor_b, &payload);
    smoke.check(echoed == payload, "monitor A to B carries 4096 bytes");
    let echoed = transfer(monitor_b, monitor_a, &payload);
    smoke.check(echoed == payload, "monitor B to A carries 4096 bytes");
    smoke.check(
        unsafe { c0c_monitor_dropped(monitor_a) } == 0,
        "monitor reports no dropped chunks",
    );
    unsafe {
        c0c_monitor_close(monitor_b);
        c0c_monitor_close(monitor_a);
    }

    let mut required = 0usize;
    let code = unsafe { c0c_state_export_json(session, ptr::null_mut(), 0, &mut required) };
    smoke.check(
        code == C0C_E_BUFFER_TOO_SMALL && required > 0,
        "state export reports the required size",
    );
    let mut json = vec![0 as c_char; required];
    let code =
        unsafe { c0c_state_export_json(session, json.as_mut_ptr(), json.len(), &mut required) };
    let json = read(&json);
    smoke.check(
        code == C0C_OK && json.contains("\"schema\": 2") && json.contains("COM11"),
        "state export json",
    );

    let restored = open_fake()?;
    let options = c0c_restore_options {
        dry_run: 0,
        remove_extra: 0,
        continue_on_error: 0,
    };
    let mut report = c0c_restore_report::default();
    let json_c = CString::new(json).unwrap();
    let code = unsafe { c0c_state_import_json(restored, json_c.as_ptr(), &options, &mut report) };
    smoke.check(
        code == C0C_OK && report.applied == 3 && report.failed == 0,
        "state import applies three pairs",
    );
    let mut pairs = vec![c0c_pair_info::default(); 3];
    let mut count = 0;
    let code = unsafe { c0c_list_pairs(restored, pairs.as_mut_ptr(), pairs.len(), &mut count) };
    smoke.check(
        code == C0C_OK && count == 3,
        "imported session has three pairs",
    );
    unsafe {
        c0c_close(restored);
        c0c_close(session);
    }
    Ok(())
}

/// Real driver run: diagnostics, pair CRUD, parameters, events and monitors.
fn run_native(smoke: &mut Smoke) -> Result<(), String> {
    let session = open(C0C_BACKEND_NATIVE)?;
    let result = run_native_session(smoke, session);
    unsafe {
        c0c_close(session);
    }
    result
}

fn run_native_session(smoke: &mut Smoke, session: *mut c0c_session) -> Result<(), String> {
    let mut info = c0c_driver_desc::default();
    let code = unsafe { c0c_driver_info(session, C0C_DRIVER_BUS, &mut info) };
    smoke.check(
        code == C0C_OK && info.status == C0C_DRIVER_RUNNING && read(&info.sha256).len() == 64,
        "native bus driver is running with a SHA-256",
    );

    let mut logs = vec![c0c_log_entry::default(); 16];
    let mut count = 0;
    let code = unsafe {
        c0c_driver_logs(
            session,
            logs.len() as u32,
            logs.as_mut_ptr(),
            logs.len(),
            &mut count,
        )
    };
    smoke.check(
        code == C0C_OK && count <= logs.len(),
        "native driver logs query",
    );

    let mut pairs = vec![c0c_pair_info::default(); 64];
    let mut count = 0;
    let code = unsafe { c0c_list_pairs(session, pairs.as_mut_ptr(), pairs.len(), &mut count) };
    if code != C0C_OK {
        return Err(format!("list pairs: {}", last_error(session)));
    }
    let id = (0u32..=63)
        .find(|id| !pairs[..count].iter().any(|pair| pair.id == *id))
        .ok_or("all pair ids are in use")?;

    let mut created = c0c_pair_info::default();
    let code = unsafe { c0c_create_pair(session, id as i32, &c0c_params::default(), &mut created) };
    if code != C0C_OK {
        return Err(format!("create pair {id}: {}", last_error(session)));
    }
    smoke.check(created.id == id, "native create pair");

    let result = (|| {
        let (port_a, port_b) = wait_native_ports(session, id)?;
        smoke.check(
            !port_a.is_empty() && !port_b.is_empty() && port_a != port_b,
            "native pair ports listed",
        );

        // The port driver is demand-start: it is only loaded while at least
        // one port device exists, so this check has to happen after create.
        let mut info = c0c_driver_desc::default();
        let code = unsafe { c0c_driver_info(session, C0C_DRIVER_PORT, &mut info) };
        let running =
            code == C0C_OK && info.status == C0C_DRIVER_RUNNING && read(&info.sha256).len() == 64;
        smoke.check(running, "native port driver is running with a SHA-256");
        if !running {
            return Err("native port driver is not running".into());
        }

        native_pair(smoke, session, id, &port_a, &port_b)
    })();
    if result.is_err() {
        unsafe {
            c0c_remove_pair(session, id);
        }
    }
    result
}

fn native_pair(
    smoke: &mut Smoke,
    session: *mut c0c_session,
    id: u32,
    port_a: &str,
    port_b: &str,
) -> Result<(), String> {
    let mut params = c0c_params::default();
    let code = unsafe { c0c_get_params(session, id, &mut params) };
    if code != C0C_OK {
        return Err(format!("get params: {}", last_error(session)));
    }
    smoke.check(true, "native params readable");

    let mut updated = params;
    updated.baud_emu = 1 - updated.baud_emu;
    let code = unsafe { c0c_set_params(session, id, &updated) };
    if code != C0C_OK {
        return Err(format!("set params: {}", last_error(session)));
    }
    let mut got = c0c_params::default();
    let code = unsafe { c0c_get_params(session, id, &mut got) };
    smoke.check(
        code == C0C_OK && got.baud_emu == updated.baud_emu,
        "native params roundtrip",
    );

    reset_events();
    let mut subscription: *mut c0c_subscription = ptr::null_mut();
    let code =
        unsafe { c0c_subscribe(session, Some(on_event), ptr::null_mut(), &mut subscription) };
    if code != C0C_OK || subscription.is_null() {
        return Err(format!("subscribe: {}", last_error(session)));
    }

    let mut toggled = updated;
    toggled.baud_emu = 1 - updated.baud_emu;
    let code = unsafe { c0c_set_params(session, id, &toggled) };
    let changed =
        code == C0C_OK && wait_event(C0C_EVENT_PARAMS_CHANGED as u32, id, Duration::from_secs(5));
    smoke.check(changed, "native params changed event");

    let config = c0c_port_config {
        baud_rate: 115_200,
        ..c0c_port_config::default()
    };
    let monitor_a = open_monitor_retry(session, port_a, &config)?;
    let monitor_b = match open_monitor_retry(session, port_b, &config) {
        Ok(monitor) => monitor,
        Err(error) => {
            unsafe {
                c0c_monitor_close(monitor_a);
                c0c_unsubscribe(subscription);
            }
            return Err(error);
        }
    };
    smoke.check(true, "native monitors opened");

    let forward: Vec<u8> = (0..1024u32).map(|index| (index % 251) as u8).collect();
    let backward: Vec<u8> = forward.iter().rev().copied().collect();
    let echoed = transfer(monitor_a, monitor_b, &forward);
    smoke.check(
        echoed == forward,
        "native monitor A to B carries 1024 bytes",
    );
    let echoed = transfer(monitor_b, monitor_a, &backward);
    smoke.check(
        echoed == backward,
        "native monitor B to A carries 1024 bytes",
    );
    unsafe {
        c0c_monitor_close(monitor_b);
        c0c_monitor_close(monitor_a);
    }

    let code = unsafe { c0c_remove_pair(session, id) };
    if code != C0C_OK {
        unsafe {
            c0c_unsubscribe(subscription);
        }
        return Err(format!("remove pair {id}: {}", last_error(session)));
    }
    let removed = wait_event(C0C_EVENT_PAIR_REMOVED as u32, id, Duration::from_secs(5));
    unsafe {
        c0c_unsubscribe(subscription);
    }
    smoke.check(removed, "native pair removed event");

    let mut pairs = vec![c0c_pair_info::default(); 64];
    let mut count = 0;
    let code = unsafe { c0c_list_pairs(session, pairs.as_mut_ptr(), pairs.len(), &mut count) };
    smoke.check(
        code == C0C_OK && !pairs[..count].iter().any(|pair| pair.id == id),
        "native pair list no longer has the pair",
    );
    Ok(())
}

/// Windows assigns auto names in PnP, so the effective names only show up in
/// `list_ports`; `get_params` keeps the empty auto marker.
fn wait_native_ports(session: *mut c0c_session, id: u32) -> Result<(String, String), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut ports = vec![c0c_port_info::default(); 64];
        let mut count = 0;
        let code = unsafe { c0c_list_ports(session, ports.as_mut_ptr(), ports.len(), &mut count) };
        if code == C0C_OK {
            let mut names: [Option<String>; 2] = [None, None];
            for port in ports.iter().take(count.min(ports.len())) {
                if port.managed != 1 || port.pair_id != id as i32 {
                    continue;
                }
                match port.port_index {
                    0 => names[0] = Some(read(&port.name)),
                    1 => names[1] = Some(read(&port.name)),
                    _ => {}
                }
            }
            if let [Some(a), Some(b)] = names {
                return Ok((a, b));
            }
        }
        if Instant::now() >= deadline {
            return Err("pair ports were not listed within 20s".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn open_monitor_retry(
    session: *mut c0c_session,
    port: &str,
    config: &c0c_port_config,
) -> Result<*mut c0c_monitor, String> {
    let name = CString::new(port).map_err(|error| format!("port {port}: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut monitor: *mut c0c_monitor = ptr::null_mut();
        let code = unsafe { c0c_open_monitor(session, name.as_ptr(), config, &mut monitor) };
        if code == C0C_OK && !monitor.is_null() {
            return Ok(monitor);
        }
        if Instant::now() >= deadline {
            return Err(format!("open {port}: {code} ({})", last_error(session)));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn reset_events() {
    EVENT_KIND.store(u32::MAX, Ordering::SeqCst);
    EVENT_PAIR.store(u32::MAX, Ordering::SeqCst);
    EVENT_COUNT.store(0, Ordering::SeqCst);
}

fn wait_event(kind: u32, pair: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if EVENT_KIND.load(Ordering::SeqCst) == kind && EVENT_PAIR.load(Ordering::SeqCst) == pair {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Write `payload` into `writer` and collect it from `reader` until the full
/// payload arrives or the 3 second deadline expires.
fn transfer(writer: *mut c0c_monitor, reader: *mut c0c_monitor, payload: &[u8]) -> Vec<u8> {
    let mut written = 0usize;
    let code = unsafe { c0c_monitor_write(writer, payload.as_ptr(), payload.len(), &mut written) };
    if code != C0C_OK || written != payload.len() {
        return Vec::new();
    }
    let mut received = Vec::with_capacity(payload.len());
    let deadline = Instant::now() + Duration::from_secs(3);
    while received.len() < payload.len() && Instant::now() < deadline {
        let mut buffer = [0u8; 1024];
        let mut read = 0usize;
        let code =
            unsafe { c0c_monitor_read(reader, buffer.as_mut_ptr(), buffer.len(), 500, &mut read) };
        if code != C0C_OK {
            break;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    received
}
