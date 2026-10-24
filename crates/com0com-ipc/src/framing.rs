//! Newline framing shared by every transport.
//!
//! A frame is one JSON document terminated by `\n`. Readers accumulate bytes
//! until a newline and reject frames above [`MAX_FRAME_SIZE`]. `\r\n` is
//! tolerated so a hand-written client on Windows still works.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use com0com_core::SdkError;
use serde_json::Value;

use crate::protocol::{Notification, Response, RpcError};
use crate::MAX_FRAME_SIZE;

/// Blocking frame source. `Ok(None)` means the peer closed the connection
/// cleanly (or the transport was stopped).
pub trait FrameReader: Send {
    fn read_frame(&mut self) -> Result<Option<Vec<u8>>, SdkError>;
}

/// Blocking frame sink. Implementations must write the whole frame or fail.
pub trait FrameWriter: Send {
    fn write_frame(&mut self, frame: &[u8]) -> Result<(), SdkError>;
}

pub type SharedWriter = Arc<Mutex<Box<dyn FrameWriter>>>;

pub struct Connection {
    pub reader: Box<dyn FrameReader>,
    pub writer: SharedWriter,
    /// Set before dropping a client connection so a polling reader thread
    /// finishes promptly instead of blocking on the next byte.
    pub stop: Arc<AtomicBool>,
    /// Unix peer uid (`SO_PEERCRED` / `getpeereid`); always `None` on Windows
    /// and on in-memory test connections.
    pub peer_uid: Option<u32>,
}

impl Connection {
    pub fn new(reader: impl FrameReader + 'static, writer: impl FrameWriter + 'static) -> Self {
        Self {
            reader: Box::new(reader),
            writer: Arc::new(Mutex::new(Box::new(writer))),
            stop: Arc::new(AtomicBool::new(false)),
            peer_uid: None,
        }
    }
}

/// Extract one complete frame from `buffer`, if present.
pub(crate) fn take_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, SdkError> {
    if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut frame: Vec<u8> = buffer.drain(..=newline).collect();
        frame.pop();
        if frame.last() == Some(&b'\r') {
            frame.pop();
        }
        if frame.len() > MAX_FRAME_SIZE {
            return Err(SdkError::Protocol(format!(
                "frame is {} bytes, limit is {MAX_FRAME_SIZE}",
                frame.len()
            )));
        }
        return Ok(Some(frame));
    }
    if buffer.len() > MAX_FRAME_SIZE {
        return Err(SdkError::Protocol(format!(
            "frame exceeds the {MAX_FRAME_SIZE}-byte limit"
        )));
    }
    Ok(None)
}

pub fn write_json(writer: &SharedWriter, value: &Value) -> Result<(), SdkError> {
    let frame = serde_json::to_vec(value)
        .map_err(|error| SdkError::Internal(format!("frame encoding failed: {error}")))?;
    if frame.len() > MAX_FRAME_SIZE {
        return Err(SdkError::Internal(format!(
            "outgoing frame is {} bytes, limit is {MAX_FRAME_SIZE}",
            frame.len()
        )));
    }
    writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .write_frame(&frame)
}

pub fn send_response(
    writer: &SharedWriter,
    id: Value,
    result: Result<Value, RpcError>,
) -> Result<(), SdkError> {
    let response = match result {
        Ok(value) => Response::ok(id, value),
        Err(error) => Response::err(id, error),
    };
    write_json(writer, &response.to_value())
}

pub fn send_notification(
    writer: &SharedWriter,
    method: &'static str,
    params: Value,
) -> Result<(), SdkError> {
    write_json(writer, &Notification::new(method, params).to_value())
}

/// Append `frame` plus the newline delimiter for transports that write bytes.
pub(crate) fn framed_bytes(frame: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frame.len() + 1);
    bytes.extend_from_slice(frame);
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_frame_handles_newlines_and_limits() {
        let mut buffer = b"{\"a\":1}\r\n{\"b\":2}".to_vec();
        assert_eq!(take_frame(&mut buffer).unwrap().unwrap(), b"{\"a\":1}");
        assert!(take_frame(&mut buffer).unwrap().is_none());
        buffer.push(b'\n');
        assert_eq!(take_frame(&mut buffer).unwrap().unwrap(), b"{\"b\":2}");
        assert!(buffer.is_empty());

        let mut oversized = vec![b'x'; MAX_FRAME_SIZE + 1];
        assert!(matches!(
            take_frame(&mut oversized),
            Err(SdkError::Protocol(_))
        ));
    }

    #[test]
    fn framed_bytes_appends_one_newline() {
        assert_eq!(framed_bytes(b"abc"), b"abc\n");
    }
}
