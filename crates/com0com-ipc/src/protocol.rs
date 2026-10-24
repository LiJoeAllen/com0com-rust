//! Wire types for the newline-delimited JSON-RPC 2.0 protocol.
//!
//! Every frame is one JSON object terminated by `\n`. Requests and responses
//! carry `jsonrpc: "2.0"`; the server sends notifications for events, serial
//! data and serial closure. The first frame on every connection must be
//! [`METHOD_HELLO`].

use com0com_core::{
    BackendInfo, Capabilities, DriverInfo, DriverKind, Event, LogEntry, Pair, PairId, PairParams,
    PortConfig, PortInfo, RestoreOptions, RestoreReport, Snapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const JSONRPC_VERSION: &str = "2.0";

pub const METHOD_HELLO: &str = "rpc.hello";
pub const METHOD_PING: &str = "rpc.ping";
pub const METHOD_SHUTDOWN: &str = "rpc.shutdown";
pub const METHOD_DRIVER_INFO: &str = "driver.info";
pub const METHOD_DRIVER_LOGS: &str = "driver.logs";
pub const METHOD_CAPABILITIES: &str = "driver.capabilities";
pub const METHOD_PAIRS_LIST: &str = "pairs.list";
pub const METHOD_PAIRS_GET: &str = "pairs.get";
pub const METHOD_PAIRS_CREATE: &str = "pairs.create";
pub const METHOD_PAIRS_DELETE: &str = "pairs.delete";
pub const METHOD_PAIRS_SET_PARAMS: &str = "pairs.set_params";
pub const METHOD_PAIRS_RESET_PARAMS: &str = "pairs.reset_params";
pub const METHOD_PORTS_LIST: &str = "ports.list";
pub const METHOD_EVENTS_SUBSCRIBE: &str = "events.subscribe";
pub const METHOD_EVENTS_UNSUBSCRIBE: &str = "events.unsubscribe";
pub const METHOD_SERIAL_OPEN: &str = "serial.open";
pub const METHOD_SERIAL_WRITE: &str = "serial.write";
pub const METHOD_SERIAL_CLOSE: &str = "serial.close";
pub const METHOD_STATE_SNAPSHOT: &str = "state.snapshot";
pub const METHOD_STATE_SAVE: &str = "state.save";
pub const METHOD_STATE_LOAD: &str = "state.load";
pub const METHOD_STATE_EXPORT: &str = "state.export";
pub const METHOD_STATE_IMPORT: &str = "state.import";
pub const METHOD_STATE_RESTORE: &str = "state.restore";
pub const METHOD_DAEMON_STATUS: &str = "daemon.status";
pub const METHOD_DAEMON_LOGS: &str = "daemon.logs";

pub const NOTIFY_EVENT: &str = "event";
pub const NOTIFY_SERIAL_DATA: &str = "serial.data";
pub const NOTIFY_SERIAL_CLOSED: &str = "serial.closed";

// Application error codes. The standard JSON-RPC range is used for framing and
// parameter problems; -32000..=-32009 are backend errors.
pub const CODE_IO: i64 = -32000;
pub const CODE_PROTOCOL: i64 = -32001;
pub const CODE_PERMISSION: i64 = -32002;
pub const CODE_UNSUPPORTED: i64 = -32003;
pub const CODE_NOT_FOUND: i64 = -32004;
pub const CODE_ALREADY_EXISTS: i64 = -32005;
pub const CODE_DAEMON_UNAVAILABLE: i64 = -32006;
pub const CODE_BUSY: i64 = -32007;
pub const CODE_DRIVER_UNAVAILABLE: i64 = -32008;
pub const CODE_TIMEOUT: i64 = -32009;
pub const CODE_PARSE: i64 = -32700;
pub const CODE_INVALID_REQUEST: i64 = -32600;
pub const CODE_METHOD_NOT_FOUND: i64 = -32601;
pub const CODE_INVALID_PARAMS: i64 = -32602;
pub const CODE_INTERNAL: i64 = -32603;

/// One parsed request. `id` is kept as raw JSON so string and numeric ids both
/// round-trip unchanged; `Value::Null` marks an invalid request.
#[derive(Clone, Debug)]
pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

impl Request {
    pub fn parse(frame: &[u8]) -> Result<Self, RpcError> {
        let value: Value = serde_json::from_slice(frame)
            .map_err(|error| RpcError::new(CODE_PARSE, format!("invalid JSON frame: {error}")))?;
        let Some(object) = value.as_object() else {
            return Err(RpcError::new(
                CODE_INVALID_REQUEST,
                "request must be a JSON object",
            ));
        };
        if let Some(version) = object.get("jsonrpc") {
            if version.as_str() != Some(JSONRPC_VERSION) {
                return Err(RpcError::new(
                    CODE_INVALID_REQUEST,
                    "jsonrpc must be \"2.0\"",
                ));
            }
        }
        let id = object.get("id").cloned().unwrap_or(Value::Null);
        if id.is_object() || id.is_array() {
            return Err(RpcError::new(
                CODE_INVALID_REQUEST,
                "id must be a string, number or null",
            ));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Err(RpcError::new(
                CODE_INVALID_REQUEST,
                "request is missing a method",
            ));
        };
        let params = object
            .get("params")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        Ok(Self {
            id,
            method: method.to_string(),
            params,
        })
    }

    pub fn id_u64(&self) -> Option<u64> {
        self.id.as_u64()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn invalid_params(error: impl std::fmt::Display) -> Self {
        Self::new(CODE_INVALID_PARAMS, format!("invalid params: {error}"))
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(CODE_METHOD_NOT_FOUND, format!("unknown method {method:?}"))
    }

    /// Map a backend error to the stable wire code.
    pub fn from_sdk(error: &com0com_core::SdkError) -> Self {
        use com0com_core::SdkError;
        let code = match error {
            SdkError::Io(_) | SdkError::Win32 { .. } => CODE_IO,
            SdkError::Protocol(_) => CODE_PROTOCOL,
            SdkError::PermissionDenied => CODE_PERMISSION,
            SdkError::Unsupported => CODE_UNSUPPORTED,
            SdkError::NotFound => CODE_NOT_FOUND,
            SdkError::AlreadyExists => CODE_ALREADY_EXISTS,
            SdkError::DaemonUnavailable => CODE_DAEMON_UNAVAILABLE,
            SdkError::Busy => CODE_BUSY,
            SdkError::DriverUnavailable => CODE_DRIVER_UNAVAILABLE,
            SdkError::Timeout => CODE_TIMEOUT,
            SdkError::InvalidArgument(_) => CODE_INVALID_PARAMS,
            SdkError::Internal(_) => CODE_INTERNAL,
        };
        let mut rpc = Self::new(code, error.to_string()).with_data(json!({
            "kind": format!("{:?}", error.kind()),
        }));
        if let Some(win32) = error.win32_code() {
            rpc.data = Some(json!({"kind": "Win32", "win32": win32}));
        }
        rpc
    }

    /// Rebuild an [`SdkError`] for the client facade.
    pub fn to_sdk(&self) -> com0com_core::SdkError {
        use com0com_core::SdkError;
        match self.code {
            CODE_IO => SdkError::Io(self.message.clone()),
            CODE_PROTOCOL => SdkError::Protocol(self.message.clone()),
            CODE_PERMISSION => SdkError::PermissionDenied,
            CODE_UNSUPPORTED => SdkError::Unsupported,
            CODE_NOT_FOUND => SdkError::NotFound,
            CODE_ALREADY_EXISTS => SdkError::AlreadyExists,
            CODE_DAEMON_UNAVAILABLE => SdkError::DaemonUnavailable,
            CODE_BUSY => SdkError::Busy,
            CODE_DRIVER_UNAVAILABLE => SdkError::DriverUnavailable,
            CODE_TIMEOUT => SdkError::Timeout,
            CODE_INVALID_PARAMS => SdkError::InvalidArgument(self.message.clone()),
            _ => SdkError::Internal(self.message.clone()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"response encoding failed"}}))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: Value,
}

impl Notification {
    pub fn new(method: &'static str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            method,
            params,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol: u32,
    #[serde(default)]
    pub client: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol: u32,
    pub server: String,
    pub version: String,
    pub capabilities: Capabilities,
    pub backend: BackendInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PingResult {
    pub pong: bool,
    pub protocol: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairIdParams {
    pub id: PairId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatePairParams {
    #[serde(default)]
    pub id: Option<PairId>,
    #[serde(default)]
    pub params: PairParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetParamsParams {
    pub id: PairId,
    pub params: PairParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairResult {
    pub pair: Pair,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairsResult {
    pub pairs: Vec<Pair>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParamsResult {
    pub params: PairParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortsResult {
    pub ports: Vec<PortInfo>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SubscribeParams {
    /// `Default` is [`com0com_core::EventFilter::all`].
    #[serde(default)]
    pub filter: com0com_core::EventFilter,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscribeResult {
    pub subscription: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnsubscribeParams {
    pub subscription: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventParams {
    pub subscription: u64,
    pub event: Event,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverInfoParams {
    pub kind: DriverKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverInfoResult {
    pub info: DriverInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogsParams {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogsResult {
    pub entries: Vec<LogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilitiesResult {
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenSerialParams {
    pub port: String,
    #[serde(default)]
    pub config: PortConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenSerialResult {
    pub handle: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerialWriteParams {
    pub handle: u64,
    /// Standard base64 (with padding).
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerialWriteResult {
    pub written: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerialHandleParams {
    pub handle: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerialDataParams {
    pub handle: u64,
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerialClosedParams {
    pub handle: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateSnapshotResult {
    pub snapshot: Snapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateSaveResult {
    pub snapshot: Snapshot,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateExportResult {
    pub json: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateImportParams {
    pub json: String,
    #[serde(default)]
    pub options: RestoreOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateRestoreParams {
    pub snapshot: Snapshot,
    #[serde(default)]
    pub options: RestoreOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreResult {
    pub report: RestoreReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonStatusResult {
    pub running: bool,
    pub pid: u32,
    pub version: String,
    pub protocol: u32,
    pub connections: usize,
    pub uptime_ms: u64,
    pub state_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests_and_rejects_malformed_frames() {
        let request = Request::parse(br#"{"jsonrpc":"2.0","id":7,"method":"pairs.list"}"#).unwrap();
        assert_eq!(request.id_u64(), Some(7));
        assert_eq!(request.method, METHOD_PAIRS_LIST);
        assert!(request.params.is_object());

        assert_eq!(Request::parse(b"{").unwrap_err().code, CODE_PARSE);
        assert_eq!(
            Request::parse(br#"{"jsonrpc":"1.0","id":1,"method":"x"}"#)
                .unwrap_err()
                .code,
            CODE_INVALID_REQUEST
        );
        assert_eq!(
            Request::parse(br#"{"jsonrpc":"2.0","id":1}"#)
                .unwrap_err()
                .code,
            CODE_INVALID_REQUEST
        );
        assert_eq!(
            Request::parse(br#"{"jsonrpc":"2.0","id":{},"method":"x"}"#)
                .unwrap_err()
                .code,
            CODE_INVALID_REQUEST
        );
    }

    #[test]
    fn sdk_errors_roundtrip_through_rpc_codes() {
        use com0com_core::SdkError;
        for (error, code) in [
            (SdkError::NotFound, CODE_NOT_FOUND),
            (SdkError::AlreadyExists, CODE_ALREADY_EXISTS),
            (SdkError::Busy, CODE_BUSY),
            (SdkError::PermissionDenied, CODE_PERMISSION),
            (SdkError::Unsupported, CODE_UNSUPPORTED),
            (SdkError::DriverUnavailable, CODE_DRIVER_UNAVAILABLE),
            (SdkError::Timeout, CODE_TIMEOUT),
            (SdkError::DaemonUnavailable, CODE_DAEMON_UNAVAILABLE),
        ] {
            let rpc = RpcError::from_sdk(&error);
            assert_eq!(rpc.code, code);
            assert_eq!(rpc.to_sdk().kind(), error.kind());
        }
        let rpc = RpcError::from_sdk(&SdkError::Win32 {
            op: "CreateFile".into(),
            code: 5,
            hint: None,
        });
        assert_eq!(rpc.code, CODE_IO);
        assert_eq!(rpc.data.unwrap()["win32"], 5);
    }

    #[test]
    fn response_and_notification_serialize_as_jsonrpc() {
        let response = Response::ok(json!(1), json!({"ok": true})).to_value();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["result"]["ok"], true);
        assert!(response.get("error").is_none());

        let error = Response::err(Value::Null, RpcError::new(CODE_PARSE, "bad")).to_value();
        assert_eq!(error["error"]["code"], CODE_PARSE);

        let notification = Notification::new(NOTIFY_EVENT, json!({"subscription": 1}));
        assert_eq!(notification.to_value()["method"], NOTIFY_EVENT);
    }
}
