//! JSON-RPC client: request/response correlation, notification routing and
//! [`RemoteBackend`], the [`Backend`] implementation used by
//! `Sdk::connect_daemon`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine as _;
use com0com_core::{
    Backend, BackendInfo, Capabilities, DriverInfo, DriverKind, Event, EventFilter,
    EventSubscription, LogEntry, MonitorEvent, MonitorHandle, Pair, PairId, PairParams, PortConfig,
    PortInfo, PortName, RestoreOptions, RestoreReport, SdkError, Snapshot,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::framing::{write_json, FrameReader, SharedWriter};
use crate::protocol::*;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_QUEUE: usize = 64;
const MONITOR_QUEUE: usize = 256;

enum Listener {
    Event(SyncSender<Event>, Arc<AtomicU64>),
    Serial(SyncSender<MonitorEvent>, Arc<AtomicU64>),
}

type Pending = Arc<Mutex<HashMap<u64, SyncSender<Result<Value, RpcError>>>>>;
type Listeners = Arc<Mutex<HashMap<(u8, u64), Listener>>>;

const LISTENER_EVENT: u8 = 0;
const LISTENER_SERIAL: u8 = 1;

/// A connected JSON-RPC client. Cloning is not supported; wrap it in `Arc`.
pub struct Client {
    writer: SharedWriter,
    pending: Pending,
    listeners: Listeners,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    reader: Mutex<Option<JoinHandle<()>>>,
    hello: OnceLock<HelloResult>,
}

impl Client {
    /// Connect to `endpoint` (or the platform default) and perform the
    /// `rpc.hello` handshake.
    pub fn connect(endpoint: Option<&str>) -> Result<Self, SdkError> {
        Self::connect_with_timeout(endpoint, REQUEST_TIMEOUT)
    }

    pub fn connect_with_timeout(
        endpoint: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, SdkError> {
        let connection = crate::transport::connect(endpoint, timeout)?;
        let stop = Arc::clone(&connection.stop);
        let writer = connection.writer;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let listeners: Listeners = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));

        let reader_pending = Arc::clone(&pending);
        let reader_listeners = Arc::clone(&listeners);
        let reader_closed = Arc::clone(&closed);
        let reader = connection.reader;
        let handle = thread::Builder::new()
            .name("com0com-rpc-reader".into())
            .spawn(move || reader_loop(reader, reader_pending, reader_listeners, reader_closed))
            .map_err(|error| SdkError::Internal(format!("spawn RPC reader: {error}")))?;

        let client = Self {
            writer,
            pending,
            listeners,
            next_id: AtomicU64::new(1),
            closed,
            stop,
            reader: Mutex::new(Some(handle)),
            hello: OnceLock::new(),
        };
        let value = client.call_json(
            METHOD_HELLO,
            json!({
                "protocol": crate::PROTOCOL_VERSION,
                "client": format!("com0com-ipc/{}", env!("CARGO_PKG_VERSION")),
            }),
        )?;
        let hello: HelloResult = serde_json::from_value(value)
            .map_err(|error| SdkError::Protocol(format!("invalid hello response: {error}")))?;
        let _ = client.hello.set(hello);
        Ok(client)
    }

    pub fn hello(&self) -> &HelloResult {
        self.hello
            .get()
            .expect("a connected client always completes the handshake")
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Send one request and wait for its response.
    pub fn call_json(&self, method: &str, params: Value) -> Result<Value, SdkError> {
        if self.is_closed() {
            return Err(SdkError::DaemonUnavailable);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, sender);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if write_json(&self.writer, &request).is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            self.closed.store(true, Ordering::SeqCst);
            return Err(SdkError::DaemonUnavailable);
        }
        match receiver.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error.to_sdk()),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&id);
                Err(SdkError::Timeout)
            }
        }
    }

    fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, SdkError> {
        let value = self.call_json(method, params)?;
        serde_json::from_value(value)
            .map_err(|error| SdkError::Protocol(format!("invalid {method} response: {error}")))
    }

    fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError> {
        let result: SubscribeResult = self.call(
            METHOD_EVENTS_SUBSCRIBE,
            serde_json::to_value(SubscribeParams { filter })
                .map_err(|error| SdkError::Internal(error.to_string()))?,
        )?;
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        self.listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                (LISTENER_EVENT, result.subscription),
                Listener::Event(sender, Arc::clone(&dropped)),
            );
        Ok(EventSubscription::new(
            result.subscription,
            receiver,
            dropped,
        ))
    }

    fn open_monitor(
        &self,
        port: &PortName,
        cfg: &PortConfig,
    ) -> Result<(u64, Receiver<MonitorEvent>, Arc<AtomicU64>), SdkError> {
        let result: OpenSerialResult = self.call(
            METHOD_SERIAL_OPEN,
            json!({"port": port.as_str(), "config": cfg}),
        )?;
        let (sender, receiver) = mpsc::sync_channel(MONITOR_QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        self.listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                (LISTENER_SERIAL, result.handle),
                Listener::Serial(sender, Arc::clone(&dropped)),
            );
        Ok((result.handle, receiver, dropped))
    }

    fn remove_listener(&self, kind: u8, id: u64) {
        self.listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(kind, id));
    }

    pub fn shutdown(&self) -> Result<(), SdkError> {
        let _: Value = self.call(METHOD_SHUTDOWN, json!({}))?;
        Ok(())
    }

    pub fn ping(&self) -> Result<PingResult, SdkError> {
        self.call(METHOD_PING, json!({}))
    }

    pub fn status(&self) -> Result<DaemonStatusResult, SdkError> {
        self.call(METHOD_DAEMON_STATUS, json!({}))
    }

    pub fn logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        let result: LogsResult = self.call(METHOD_DAEMON_LOGS, json!({"limit": limit}))?;
        Ok(result.entries)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self
            .reader
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }
}

fn reader_loop(
    mut reader: Box<dyn FrameReader>,
    pending: Pending,
    listeners: Listeners,
    closed: Arc<AtomicBool>,
) {
    loop {
        let frame = match reader.read_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) => break,
        };
        if frame.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_slice(&frame) {
            Ok(value) => value,
            Err(_) => break,
        };
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            let outcome = match value.get("error") {
                Some(error) => Err(serde_json::from_value::<RpcError>(error.clone())
                    .unwrap_or_else(|_| RpcError::new(CODE_PROTOCOL, "malformed error response"))),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            if let Some(sender) = pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id)
            {
                let _ = sender.send(outcome);
            }
            continue;
        }
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            continue;
        };
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        match method {
            NOTIFY_EVENT => {
                if let Ok(params) = serde_json::from_value::<EventParams>(params) {
                    deliver_event(&listeners, params.subscription, params.event);
                }
            }
            NOTIFY_SERIAL_DATA => {
                let Ok(params) = serde_json::from_value::<SerialDataParams>(params) else {
                    continue;
                };
                let Ok(data) =
                    base64::engine::general_purpose::STANDARD.decode(params.data.as_bytes())
                else {
                    continue;
                };
                deliver_serial(&listeners, params.handle, MonitorEvent::Data(data), false);
            }
            NOTIFY_SERIAL_CLOSED => {
                let Ok(params) = serde_json::from_value::<SerialClosedParams>(params) else {
                    continue;
                };
                let event = match params.reason {
                    Some(reason) => MonitorEvent::Error(reason),
                    None => MonitorEvent::Closed,
                };
                deliver_serial(&listeners, params.handle, event, true);
            }
            _ => {}
        }
    }

    closed.store(true, Ordering::SeqCst);
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(RpcError::new(
            CODE_DAEMON_UNAVAILABLE,
            "daemon connection closed",
        )));
    }
    listeners
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn deliver_event(listeners: &Listeners, id: u64, event: Event) {
    let listeners = listeners
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(Listener::Event(sender, dropped)) = listeners.get(&(LISTENER_EVENT, id)) {
        if sender.try_send(event).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn deliver_serial(listeners: &Listeners, id: u64, event: MonitorEvent, terminal: bool) {
    let mut listeners = listeners
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = if terminal {
        listeners.remove(&(LISTENER_SERIAL, id))
    } else {
        listeners.get(&(LISTENER_SERIAL, id)).map(|listener| {
            // SAFETY-free re-borrow: clone the sender/counter out instead.
            match listener {
                Listener::Serial(sender, dropped) => {
                    Listener::Serial(sender.clone(), Arc::clone(dropped))
                }
                Listener::Event(..) => unreachable!(),
            }
        })
    };
    if let Some(Listener::Serial(sender, dropped)) = entry {
        if sender.try_send(event).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// [`Backend`] backed by a local daemon connection.
pub struct RemoteBackend {
    client: Arc<Client>,
}

impl RemoteBackend {
    pub fn connect(endpoint: Option<&str>) -> Result<Self, SdkError> {
        Self::connect_with_timeout(endpoint, REQUEST_TIMEOUT)
    }

    pub fn connect_with_timeout(
        endpoint: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, SdkError> {
        Ok(Self {
            client: Arc::new(Client::connect_with_timeout(endpoint, timeout)?),
        })
    }

    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    pub fn snapshot(&self) -> Result<Snapshot, SdkError> {
        let result: StateSnapshotResult = self.client.call(METHOD_STATE_SNAPSHOT, json!({}))?;
        Ok(result.snapshot)
    }

    pub fn save_state(&self) -> Result<StateSaveResult, SdkError> {
        self.client.call(METHOD_STATE_SAVE, json!({}))
    }

    pub fn load_state(&self) -> Result<Snapshot, SdkError> {
        let result: StateSnapshotResult = self.client.call(METHOD_STATE_LOAD, json!({}))?;
        Ok(result.snapshot)
    }

    pub fn export_state(&self) -> Result<String, SdkError> {
        let result: StateExportResult = self.client.call(METHOD_STATE_EXPORT, json!({}))?;
        Ok(result.json)
    }

    pub fn import_state(
        &self,
        json: &str,
        options: RestoreOptions,
    ) -> Result<RestoreReport, SdkError> {
        let result: RestoreResult = self.client.call(
            METHOD_STATE_IMPORT,
            json!({"json": json, "options": options}),
        )?;
        Ok(result.report)
    }

    pub fn restore_state(
        &self,
        snapshot: &Snapshot,
        options: RestoreOptions,
    ) -> Result<RestoreReport, SdkError> {
        let result: RestoreResult = self.client.call(
            METHOD_STATE_RESTORE,
            json!({"snapshot": snapshot, "options": options}),
        )?;
        Ok(result.report)
    }

    pub fn status(&self) -> Result<DaemonStatusResult, SdkError> {
        self.client.status()
    }

    pub fn logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        self.client.logs(limit)
    }

    pub fn shutdown(&self) -> Result<(), SdkError> {
        self.client.shutdown()
    }
}

impl Backend for RemoteBackend {
    fn info(&self) -> BackendInfo {
        self.client.hello().backend.clone()
    }

    fn capabilities(&self) -> Capabilities {
        self.client.hello().capabilities | Capabilities::REMOTE_DAEMON
    }

    fn list_pairs(&self) -> Result<Vec<Pair>, SdkError> {
        let result: PairsResult = self.client.call(METHOD_PAIRS_LIST, json!({}))?;
        Ok(result.pairs)
    }

    fn create_pair(&self, id: Option<PairId>, params: &PairParams) -> Result<Pair, SdkError> {
        let result: PairResult = self
            .client
            .call(METHOD_PAIRS_CREATE, json!({"id": id, "params": params}))?;
        Ok(result.pair)
    }

    fn delete_pair(&self, id: PairId) -> Result<(), SdkError> {
        let _: Value = self.client.call(METHOD_PAIRS_DELETE, json!({"id": id}))?;
        Ok(())
    }

    fn get_params(&self, id: PairId) -> Result<PairParams, SdkError> {
        let result: PairResult = self.client.call(METHOD_PAIRS_GET, json!({"id": id}))?;
        Ok(result.pair.params)
    }

    fn set_params(&self, id: PairId, params: &PairParams) -> Result<(), SdkError> {
        let _: PairResult = self
            .client
            .call(METHOD_PAIRS_SET_PARAMS, json!({"id": id, "params": params}))?;
        Ok(())
    }

    fn list_ports(&self) -> Result<Vec<PortInfo>, SdkError> {
        let result: PortsResult = self.client.call(METHOD_PORTS_LIST, json!({}))?;
        Ok(result.ports)
    }

    fn open_monitor(
        &self,
        port: &PortName,
        cfg: &PortConfig,
    ) -> Result<Box<dyn MonitorHandle>, SdkError> {
        let (handle, receiver, dropped) = self.client.open_monitor(port, cfg)?;
        Ok(Box::new(RemoteMonitor {
            client: Arc::clone(&self.client),
            handle,
            receiver,
            dropped,
        }))
    }

    fn driver_info(&self, kind: DriverKind) -> Result<DriverInfo, SdkError> {
        let result: DriverInfoResult = self
            .client
            .call(METHOD_DRIVER_INFO, json!({"kind": kind}))?;
        Ok(result.info)
    }

    fn driver_logs(&self, limit: usize) -> Result<Vec<LogEntry>, SdkError> {
        let result: LogsResult = self
            .client
            .call(METHOD_DRIVER_LOGS, json!({"limit": limit}))?;
        Ok(result.entries)
    }

    fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, SdkError> {
        self.client.subscribe(filter)
    }
}

struct RemoteMonitor {
    client: Arc<Client>,
    handle: u64,
    receiver: Receiver<MonitorEvent>,
    dropped: Arc<AtomicU64>,
}

impl MonitorHandle for RemoteMonitor {
    fn write(&mut self, data: &[u8]) -> Result<usize, SdkError> {
        let result: SerialWriteResult = self.client.call(
            METHOD_SERIAL_WRITE,
            json!({
                "handle": self.handle,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            }),
        )?;
        Ok(result.written)
    }

    fn events(&self) -> &Receiver<MonitorEvent> {
        &self.receiver
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for RemoteMonitor {
    fn drop(&mut self) {
        let _: Result<Value, SdkError> = self
            .client
            .call(METHOD_SERIAL_CLOSE, json!({"handle": self.handle}));
        self.client.remove_listener(LISTENER_SERIAL, self.handle);
    }
}

impl std::fmt::Debug for RemoteBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteBackend")
            .field("closed", &self.client.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_delivery_counts_drops() {
        let listeners: Listeners = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        listeners.lock().unwrap().insert(
            (LISTENER_EVENT, 7),
            Listener::Event(sender, Arc::clone(&dropped)),
        );
        let event = Event::PairRemoved { id: 1 };
        deliver_event(&listeners, 7, event.clone());
        deliver_event(&listeners, 7, event);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::PairRemoved { id: 1 })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn terminal_serial_notification_removes_the_listener() {
        let listeners: Listeners = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        listeners.lock().unwrap().insert(
            (LISTENER_SERIAL, 3),
            Listener::Serial(sender, Arc::clone(&dropped)),
        );
        deliver_serial(&listeners, 3, MonitorEvent::Closed, true);
        assert!(matches!(receiver.recv(), Ok(MonitorEvent::Closed)));
        deliver_serial(&listeners, 3, MonitorEvent::Data(vec![1]), false);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(listeners.lock().unwrap().is_empty());
    }
}
