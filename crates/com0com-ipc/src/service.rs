//! Server-side JSON-RPC dispatch.
//!
//! One thread serves one connection. Requests are handled serially; events and
//! serial data are pushed from per-subscription threads through the same
//! writer mutex, so a management request never has to wait for a serial read.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use com0com_core::state_io;
use com0com_core::{
    Backend, LogEntry, LogLevel, MonitorEvent, MonitorHandle, Pair, PairParams, PortName,
    RecvError, SdkError,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::framing::{send_notification, send_response, Connection, FrameReader, SharedWriter};
use crate::protocol::*;
use crate::PROTOCOL_VERSION;

const DEFAULT_LOG_LIMIT: usize = 200;
const LOG_CAPACITY: usize = 512;

/// Daemon log: a bounded in-memory ring plus an optional append-only file.
pub struct DaemonLog {
    entries: Mutex<VecDeque<LogEntry>>,
    sink: Mutex<Option<File>>,
    capacity: usize,
}

impl Default for DaemonLog {
    fn default() -> Self {
        Self::new(LOG_CAPACITY, None).unwrap_or(Self {
            entries: Mutex::new(VecDeque::new()),
            sink: Mutex::new(None),
            capacity: LOG_CAPACITY,
        })
    }
}

impl DaemonLog {
    pub fn new(capacity: usize, path: Option<&Path>) -> Result<Self, SdkError> {
        let sink = match path {
            Some(path) => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| SdkError::Io(error.to_string()))?;
                }
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|error| {
                        SdkError::Io(format!("open log {}: {error}", path.display()))
                    })?;
                Some(file)
            }
            None => None,
        };
        Ok(Self {
            entries: Mutex::new(VecDeque::with_capacity(capacity.min(4096))),
            sink: Mutex::new(sink),
            capacity: capacity.clamp(1, 4096),
        })
    }

    pub fn push(&self, level: LogLevel, message: impl Into<String>) {
        let entry = LogEntry {
            timestamp: timestamp(),
            level,
            message: message.into(),
        };
        {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            if entries.len() == self.capacity {
                entries.pop_front();
            }
            entries.push_back(entry.clone());
        }
        if let Some(file) = self.sink.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            let _ = writeln!(
                file,
                "{} {:?} {}",
                entry.timestamp, entry.level, entry.message
            );
        }
    }

    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let take = limit.min(entries.len());
        entries.iter().skip(entries.len() - take).cloned().collect()
    }
}

/// Timestamp without pulling a date library into the daemon.
fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// Shared daemon state used by every connection handler.
pub struct ServerContext {
    backend: Arc<dyn Backend>,
    state_path: PathBuf,
    state_lock: Mutex<()>,
    log: Arc<DaemonLog>,
    shutdown: Mutex<Option<Sender<()>>>,
    autosave: Mutex<Option<Sender<()>>>,
    started: Instant,
    connections: AtomicUsize,
    next_handle: AtomicU64,
}

impl ServerContext {
    pub fn new(backend: Arc<dyn Backend>, state_path: impl Into<PathBuf>) -> Self {
        Self {
            backend,
            state_path: state_path.into(),
            state_lock: Mutex::new(()),
            log: Arc::new(DaemonLog::default()),
            shutdown: Mutex::new(None),
            autosave: Mutex::new(None),
            started: Instant::now(),
            connections: AtomicUsize::new(0),
            next_handle: AtomicU64::new(1),
        }
    }

    pub fn with_log(mut self, log: Arc<DaemonLog>) -> Self {
        self.log = log;
        self
    }

    pub fn with_shutdown(mut self, sender: Sender<()>) -> Self {
        self.shutdown = Mutex::new(Some(sender));
        self
    }

    pub fn with_autosave(mut self, sender: Sender<()>) -> Self {
        self.autosave = Mutex::new(Some(sender));
        self
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    pub fn log(&self) -> &Arc<DaemonLog> {
        &self.log
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn request_shutdown(&self) {
        if let Some(sender) = self
            .shutdown
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = sender.send(());
        }
    }

    pub fn notify_autosave(&self) {
        if let Some(sender) = self
            .autosave
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = sender.send(());
        }
    }

    fn next_handle(&self) -> u64 {
        self.next_handle.fetch_add(1, Ordering::Relaxed)
    }
}

struct ConnectionGuard(Arc<ServerContext>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

struct EventForwarder {
    stop: Arc<AtomicBool>,
    _join: Option<JoinHandle<()>>,
}

struct SerialSlot {
    monitor: Arc<Mutex<Box<dyn MonitorHandle>>>,
    stop: Arc<AtomicBool>,
    _join: Option<JoinHandle<()>>,
}

struct Session {
    writer: SharedWriter,
    events: HashMap<u64, EventForwarder>,
    serials: HashMap<u64, SerialSlot>,
    shutdown_requested: bool,
}

impl Session {
    fn new(writer: SharedWriter) -> Self {
        Self {
            writer,
            events: HashMap::new(),
            serials: HashMap::new(),
            shutdown_requested: false,
        }
    }

    fn stop_background_threads(&mut self) {
        for (_, forwarder) in self.events.drain() {
            forwarder.stop.store(true, Ordering::SeqCst);
        }
        for (_, slot) in self.serials.drain() {
            slot.stop.store(true, Ordering::SeqCst);
        }
    }
}

/// Serve one accepted connection until the peer disconnects or asks the
/// daemon to shut down.
pub fn serve_connection(ctx: Arc<ServerContext>, conn: Connection) -> Result<(), SdkError> {
    ctx.connections.fetch_add(1, Ordering::Relaxed);
    let _guard = ConnectionGuard(Arc::clone(&ctx));

    let mut reader = conn.reader;
    let mut session = Session::new(conn.writer);

    let frame = match next_frame(&mut *reader)? {
        Some(frame) => frame,
        None => return Ok(()),
    };
    let request = match Request::parse(&frame) {
        Ok(request) => request,
        Err(error) => {
            send_response(&session.writer, Value::Null, Err(error))?;
            return Ok(());
        }
    };
    if request.method != METHOD_HELLO {
        send_response(
            &session.writer,
            request.id,
            Err(RpcError::new(
                CODE_PROTOCOL,
                "the first frame must be rpc.hello",
            )),
        )?;
        return Ok(());
    }
    let id = request.id.clone();
    match handshake(&ctx, request.params) {
        Ok(value) => send_response(&session.writer, id, Ok(value))?,
        Err(error) => {
            send_response(&session.writer, id, Err(error))?;
            return Ok(());
        }
    }
    ctx.log.push(LogLevel::Information, "client connected");

    let result = request_loop(&ctx, &mut session, &mut *reader);
    session.stop_background_threads();
    ctx.log.push(LogLevel::Information, "client disconnected");
    result
}

fn handshake(ctx: &ServerContext, params: Value) -> Result<Value, RpcError> {
    let params: HelloParams = serde_json::from_value(params).map_err(RpcError::invalid_params)?;
    if params.protocol != PROTOCOL_VERSION {
        return Err(RpcError::new(
            CODE_PROTOCOL,
            format!(
                "protocol {} is not supported (server speaks {PROTOCOL_VERSION})",
                params.protocol
            ),
        ));
    }
    let hello = HelloResult {
        protocol: PROTOCOL_VERSION,
        server: "com0comd".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        capabilities: ctx.backend.capabilities(),
        backend: ctx.backend.info(),
    };
    serde_json::to_value(hello).map_err(|error| RpcError::new(CODE_INTERNAL, error.to_string()))
}

fn request_loop(
    ctx: &Arc<ServerContext>,
    session: &mut Session,
    reader: &mut dyn FrameReader,
) -> Result<(), SdkError> {
    loop {
        let Some(frame) = next_frame(reader)? else {
            return Ok(());
        };
        let request = match Request::parse(&frame) {
            Ok(request) => request,
            Err(error) => {
                send_response(&session.writer, Value::Null, Err(error))?;
                continue;
            }
        };
        let id = request.id.clone();
        let result = dispatch(ctx, session, &request);
        send_response(&session.writer, id, result)?;
        if session.shutdown_requested {
            ctx.request_shutdown();
            return Ok(());
        }
    }
}

fn next_frame(reader: &mut dyn FrameReader) -> Result<Option<Vec<u8>>, SdkError> {
    loop {
        match reader.read_frame()? {
            None => return Ok(None),
            Some(frame) if frame.is_empty() => continue,
            Some(frame) => return Ok(Some(frame)),
        }
    }
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(RpcError::invalid_params)
}

fn sdk<T>(result: Result<T, SdkError>) -> Result<T, RpcError> {
    result.map_err(|error| RpcError::from_sdk(&error))
}

fn dispatch(
    ctx: &Arc<ServerContext>,
    session: &mut Session,
    request: &Request,
) -> Result<Value, RpcError> {
    match request.method.as_str() {
        METHOD_PING => Ok(json!({"pong": true, "protocol": PROTOCOL_VERSION})),
        METHOD_HELLO => {
            let hello = HelloResult {
                protocol: PROTOCOL_VERSION,
                server: "com0comd".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                capabilities: ctx.backend.capabilities(),
                backend: ctx.backend.info(),
            };
            serde_json::to_value(hello)
                .map_err(|error| RpcError::new(CODE_INTERNAL, error.to_string()))
        }
        METHOD_SHUTDOWN => {
            session.shutdown_requested = true;
            Ok(json!({"stopping": true}))
        }
        METHOD_CAPABILITIES => Ok(json!({"capabilities": ctx.backend.capabilities()})),
        METHOD_DAEMON_STATUS => Ok(json!({
            "running": true,
            "pid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
            "connections": ctx.connections(),
            "uptime_ms": ctx.uptime_ms(),
            "state_path": ctx.state_path().display().to_string(),
        })),
        METHOD_DAEMON_LOGS => {
            let params: LogsParams = parse(request.params.clone())?;
            let limit = params.limit.unwrap_or(DEFAULT_LOG_LIMIT);
            Ok(json!({"entries": ctx.log.recent(limit)}))
        }
        METHOD_DRIVER_INFO => {
            let params: DriverInfoParams = parse(request.params.clone())?;
            let info = sdk(ctx.backend.driver_info(params.kind))?;
            Ok(json!({"info": info}))
        }
        METHOD_DRIVER_LOGS => {
            let params: LogsParams = parse(request.params.clone())?;
            let limit = params.limit.unwrap_or(DEFAULT_LOG_LIMIT);
            let entries = sdk(ctx.backend.driver_logs(limit))?;
            Ok(json!({"entries": entries}))
        }
        METHOD_PAIRS_LIST => {
            let pairs = sdk(ctx.backend.list_pairs())?;
            Ok(json!({"pairs": pairs}))
        }
        METHOD_PAIRS_GET => {
            let params: PairIdParams = parse(request.params.clone())?;
            let pair = Pair {
                id: params.id,
                params: sdk(ctx.backend.get_params(params.id))?,
            };
            Ok(json!({"pair": pair}))
        }
        METHOD_PAIRS_CREATE => {
            let params: CreatePairParams = parse(request.params.clone())?;
            let pair = sdk(ctx.backend.create_pair(params.id, &params.params))?;
            ctx.notify_autosave();
            Ok(json!({"pair": pair}))
        }
        METHOD_PAIRS_DELETE => {
            let params: PairIdParams = parse(request.params.clone())?;
            sdk(ctx.backend.delete_pair(params.id))?;
            ctx.notify_autosave();
            Ok(json!({"deleted": params.id}))
        }
        METHOD_PAIRS_SET_PARAMS => {
            let params: SetParamsParams = parse(request.params.clone())?;
            sdk(ctx.backend.set_params(params.id, &params.params))?;
            ctx.notify_autosave();
            Ok(json!({"pair": Pair { id: params.id, params: params.params }}))
        }
        METHOD_PAIRS_RESET_PARAMS => {
            let params: PairIdParams = parse(request.params.clone())?;
            let defaults = PairParams::default();
            sdk(ctx.backend.set_params(params.id, &defaults))?;
            ctx.notify_autosave();
            Ok(json!({"pair": Pair { id: params.id, params: defaults }}))
        }
        METHOD_PORTS_LIST => {
            let ports = sdk(ctx.backend.list_ports())?;
            Ok(json!({"ports": ports}))
        }
        METHOD_EVENTS_SUBSCRIBE => subscribe(ctx, session, request.params.clone()),
        METHOD_EVENTS_UNSUBSCRIBE => {
            let params: UnsubscribeParams = parse(request.params.clone())?;
            if let Some(forwarder) = session.events.remove(&params.subscription) {
                forwarder.stop.store(true, Ordering::SeqCst);
            }
            Ok(json!({"unsubscribed": params.subscription}))
        }
        METHOD_SERIAL_OPEN => open_serial(ctx, session, request.params.clone()),
        METHOD_SERIAL_WRITE => {
            let params: SerialWriteParams = parse(request.params.clone())?;
            let data = base64::engine::general_purpose::STANDARD
                .decode(params.data.as_bytes())
                .map_err(|error| {
                    RpcError::new(CODE_INVALID_PARAMS, format!("data is not base64: {error}"))
                })?;
            let slot = session.serials.get(&params.handle).ok_or_else(|| {
                RpcError::new(
                    CODE_NOT_FOUND,
                    format!("serial handle {} is not open", params.handle),
                )
            })?;
            let mut monitor = slot
                .monitor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let written = sdk(monitor.write(&data))?;
            Ok(json!({"written": written}))
        }
        METHOD_SERIAL_CLOSE => {
            let params: SerialHandleParams = parse(request.params.clone())?;
            let Some(slot) = session.serials.remove(&params.handle) else {
                return Err(RpcError::new(
                    CODE_NOT_FOUND,
                    format!("serial handle {} is not open", params.handle),
                ));
            };
            slot.stop.store(true, Ordering::SeqCst);
            Ok(json!({"closed": params.handle}))
        }
        METHOD_STATE_SNAPSHOT => {
            let _guard = ctx
                .state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let snapshot = sdk(state_io::capture(&**ctx.backend()))?;
            Ok(json!({"snapshot": snapshot}))
        }
        METHOD_STATE_SAVE => {
            let _guard = ctx
                .state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let snapshot = sdk(state_io::save(&**ctx.backend(), ctx.state_path()))?;
            Ok(json!({
                "snapshot": snapshot,
                "path": ctx.state_path().display().to_string(),
            }))
        }
        METHOD_STATE_LOAD => {
            let snapshot = sdk(state_io::load(ctx.state_path()))?;
            Ok(json!({"snapshot": snapshot}))
        }
        METHOD_STATE_EXPORT => {
            let _guard = ctx
                .state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let json = sdk(state_io::export_json(&**ctx.backend()))?;
            Ok(json!({"json": json}))
        }
        METHOD_STATE_IMPORT => {
            let params: StateImportParams = parse(request.params.clone())?;
            let _guard = ctx
                .state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let report = sdk(state_io::import_json(
                &**ctx.backend(),
                &params.json,
                params.options,
            ))?;
            ctx.notify_autosave();
            Ok(json!({"report": report}))
        }
        METHOD_STATE_RESTORE => {
            let params: StateRestoreParams = parse(request.params.clone())?;
            let _guard = ctx
                .state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let report = sdk(state_io::restore(
                &**ctx.backend(),
                &params.snapshot,
                params.options,
            ))?;
            ctx.notify_autosave();
            Ok(json!({"report": report}))
        }
        other => Err(RpcError::method_not_found(other)),
    }
}

fn subscribe(
    ctx: &Arc<ServerContext>,
    session: &mut Session,
    params: Value,
) -> Result<Value, RpcError> {
    let params: SubscribeParams = parse(params)?;
    let subscription = sdk(ctx.backend.subscribe(params.filter))?;
    let id = subscription.id();
    let writer = Arc::clone(&session.writer);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = thread::Builder::new()
        .name(format!("com0com-sub-{id}"))
        .spawn(move || loop {
            if thread_stop.load(Ordering::SeqCst) {
                return;
            }
            match subscription.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    let params = EventParams {
                        subscription: id,
                        event,
                    };
                    let params = match serde_json::to_value(params) {
                        Ok(value) => value,
                        Err(_) => return,
                    };
                    if send_notification(&writer, NOTIFY_EVENT, params).is_err() {
                        return;
                    }
                }
                Err(RecvError::Timeout | RecvError::Empty) => {}
                Err(RecvError::Disconnected) => return,
            }
        })
        .map_err(|error| RpcError::new(CODE_INTERNAL, error.to_string()))?;
    session.events.insert(
        id,
        EventForwarder {
            stop,
            _join: Some(join),
        },
    );
    Ok(json!({"subscription": id}))
}

fn port_matches(managed: &str, requested: &str) -> bool {
    managed == requested || (cfg!(windows) && managed.eq_ignore_ascii_case(requested))
}

/// A managed port may be named by its published `PortInfo.name` (device name
/// on Windows, link path on Unix) or by the pair label stored in `PairParams`.
/// Labels are resolved against backend state, never used as device paths.
fn is_managed_port(ctx: &ServerContext, requested: &str) -> Result<bool, RpcError> {
    let ports = sdk(ctx.backend.list_ports())?;
    if ports
        .iter()
        .any(|info| info.managed && port_matches(&info.name, requested))
    {
        return Ok(true);
    }
    let pairs = sdk(ctx.backend.list_pairs())?;
    Ok(ports.iter().filter(|info| info.managed).any(|info| {
        let (Some(id), Some(index)) = (info.pair_id, info.port_index) else {
            return false;
        };
        let Some(pair) = pairs.iter().find(|pair| pair.id == id) else {
            return false;
        };
        let label = if index == 0 {
            &pair.params.port_a
        } else {
            &pair.params.port_b
        };
        !label.is_empty() && port_matches(label.as_str(), requested)
    }))
}

fn open_serial(
    ctx: &Arc<ServerContext>,
    session: &mut Session,
    params: Value,
) -> Result<Value, RpcError> {
    let params: OpenSerialParams = parse(params)?;
    let port = PortName::new(params.port.clone()).map_err(|error| RpcError::from_sdk(&error))?;
    if !is_managed_port(ctx, &params.port)? {
        return Err(RpcError::new(
            CODE_PERMISSION,
            "serial.open only accepts ports managed by com0com",
        )
        .with_data(json!({"port": params.port})));
    }
    params
        .config
        .validate()
        .map_err(|error| RpcError::from_sdk(&error))?;
    let monitor = sdk(ctx.backend.open_monitor(&port, &params.config))?;
    let handle = ctx.next_handle();
    let monitor = Arc::new(Mutex::new(monitor));
    let writer = Arc::clone(&session.writer);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_monitor = Arc::clone(&monitor);
    let join = thread::Builder::new()
        .name(format!("com0com-serial-{handle}"))
        .spawn(move || loop {
            if thread_stop.load(Ordering::SeqCst) {
                return;
            }
            let mut events = Vec::new();
            {
                let monitor = thread_monitor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for _ in 0..64 {
                    match monitor.try_recv() {
                        Ok(event) => events.push(event),
                        Err(RecvError::Empty | RecvError::Timeout) => break,
                        Err(RecvError::Disconnected) => {
                            events.push(MonitorEvent::Closed);
                            break;
                        }
                    }
                }
            }
            if events.is_empty() {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            for event in events {
                let (method, params, terminal) = match event {
                    MonitorEvent::Data(data) => (
                        NOTIFY_SERIAL_DATA,
                        json!({
                            "handle": handle,
                            "data": base64::engine::general_purpose::STANDARD.encode(data),
                        }),
                        false,
                    ),
                    MonitorEvent::Error(reason) => (
                        NOTIFY_SERIAL_CLOSED,
                        json!({"handle": handle, "reason": reason}),
                        true,
                    ),
                    MonitorEvent::Closed => (NOTIFY_SERIAL_CLOSED, json!({"handle": handle}), true),
                };
                if send_notification(&writer, method, params).is_err() {
                    return;
                }
                if terminal {
                    thread_stop.store(true, Ordering::SeqCst);
                    return;
                }
            }
        })
        .map_err(|error| RpcError::new(CODE_INTERNAL, error.to_string()))?;
    session.serials.insert(
        handle,
        SerialSlot {
            monitor,
            stop,
            _join: Some(join),
        },
    );
    Ok(json!({"handle": handle}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_iso_like() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn daemon_log_is_bounded_and_ordered() {
        let log = DaemonLog::new(2, None).unwrap();
        log.push(LogLevel::Information, "one");
        log.push(LogLevel::Warning, "two");
        log.push(LogLevel::Error, "three");
        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "two");
        assert_eq!(recent[1].message, "three");
        assert_eq!(log.recent(0).len(), 0);
    }
}
