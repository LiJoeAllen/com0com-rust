//! Blocking transports: Windows named pipes, Unix domain sockets and an
//! in-memory duplex used by cross-platform tests.
//!
//! Readers poll with a short timeout so a client can shut down without leaking
//! a thread stuck in a blocking read. Writes are synchronous; one connection
//! has at most one writer because every call site serializes through
//! [`SharedWriter`](crate::framing::SharedWriter).

use com0com_core::SdkError;

use crate::framing::Connection;

pub use imp::Listener;

#[cfg(windows)]
mod imp {
    pub use super::windows::{connect, Listener};
}

#[cfg(unix)]
mod imp {
    pub use super::unix::{connect, Listener};
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use std::time::Duration;

    use com0com_core::SdkError;

    use crate::framing::Connection;

    pub fn connect(_endpoint: &str, _timeout: Duration) -> Result<Connection, SdkError> {
        Err(SdkError::Unsupported)
    }

    pub struct Listener;

    impl Listener {
        pub fn bind(_endpoint: &str) -> Result<Self, SdkError> {
            Err(SdkError::Unsupported)
        }

        pub fn endpoint(&self) -> &str {
            ""
        }

        pub fn accept(&self) -> Result<Connection, SdkError> {
            Err(SdkError::Unsupported)
        }
    }
}

/// Connect to `endpoint`, falling back to the platform default.
pub fn connect(
    endpoint: Option<&str>,
    timeout: std::time::Duration,
) -> Result<Connection, SdkError> {
    match endpoint {
        Some(endpoint) => imp::connect(endpoint, timeout),
        None => imp::connect(&crate::default_endpoint(), timeout),
    }
}

/// Two connected in-memory endpoints. Used by tests to exercise framing and
/// the client/server state machines without a real pipe.
pub fn memory_pair() -> (Connection, Connection) {
    memory::pair()
}

mod memory {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use com0com_core::SdkError;

    use crate::framing::{framed_bytes, take_frame, Connection, FrameReader, FrameWriter};
    use crate::MAX_FRAME_SIZE;

    struct MemoryWriter {
        sender: SyncSender<Vec<u8>>,
    }

    impl FrameWriter for MemoryWriter {
        fn write_frame(&mut self, frame: &[u8]) -> Result<(), SdkError> {
            self.sender
                .send(framed_bytes(frame))
                .map_err(|_| SdkError::DaemonUnavailable)
        }
    }

    struct MemoryReader {
        receiver: Receiver<Vec<u8>>,
        buffer: Vec<u8>,
        stop: Arc<AtomicBool>,
    }

    impl FrameReader for MemoryReader {
        fn read_frame(&mut self) -> Result<Option<Vec<u8>>, SdkError> {
            loop {
                if let Some(frame) = take_frame(&mut self.buffer)? {
                    if frame.is_empty() {
                        continue;
                    }
                    return Ok(Some(frame));
                }
                if self.stop.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                match self.receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(bytes) => {
                        if self.buffer.len() + bytes.len() > MAX_FRAME_SIZE + 1 {
                            return Err(SdkError::Protocol("frame exceeds the limit".into()));
                        }
                        self.buffer.extend_from_slice(&bytes);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
                }
            }
        }
    }

    pub(super) fn pair() -> (Connection, Connection) {
        let (to_b, from_a) = mpsc::sync_channel::<Vec<u8>>(1024);
        let (to_a, from_b) = mpsc::sync_channel::<Vec<u8>>(1024);
        let a_stop = Arc::new(AtomicBool::new(false));
        let b_stop = Arc::new(AtomicBool::new(false));
        let a = Connection {
            reader: Box::new(MemoryReader {
                receiver: from_b,
                buffer: Vec::new(),
                stop: Arc::clone(&a_stop),
            }),
            writer: Arc::new(Mutex::new(Box::new(MemoryWriter { sender: to_b }))),
            stop: a_stop,
            peer_uid: None,
        };
        let b = Connection {
            reader: Box::new(MemoryReader {
                receiver: from_a,
                buffer: Vec::new(),
                stop: Arc::clone(&b_stop),
            }),
            writer: Arc::new(Mutex::new(Box::new(MemoryWriter { sender: to_a }))),
            stop: b_stop,
            peer_uid: None,
        };
        (a, b)
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use com0com_core::SdkError;
    use windows_sys::core::PWSTR;
    use windows_sys::Win32::errhandlingapi::GetLastError;
    use windows_sys::Win32::fileapi::{CreateFileW, ReadFile, WriteFile, OPEN_EXISTING};
    use windows_sys::Win32::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::minwinbase::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::namedpipeapi::{
        ConnectNamedPipe, CreateNamedPipeW, PeekNamedPipe, WaitNamedPipeW,
    };
    use windows_sys::Win32::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::sddl::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::securitybaseapi::GetTokenInformation;
    use windows_sys::Win32::winbase::{
        LocalFree, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::winerror::{
        ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER,
        ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    };
    use windows_sys::Win32::winnt::{
        TokenUser, FILE_ATTRIBUTE_NORMAL, GENERIC_READ, GENERIC_WRITE, HANDLE,
        PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };

    use crate::framing::{framed_bytes, take_frame, Connection, FrameReader, FrameWriter};
    use crate::MAX_FRAME_SIZE;

    const PIPE_INSTANCES: u32 = 8;
    const PIPE_BUFFER: u32 = 64 * 1024;
    const POLL_INTERVAL: Duration = Duration::from_millis(10);

    fn win32(op: &'static str, code: u32) -> SdkError {
        SdkError::Win32 {
            op: op.into(),
            code,
            hint: None,
        }
    }

    fn last_error(op: &'static str) -> SdkError {
        win32(op, unsafe { GetLastError() })
    }

    fn is_closed(code: u32) -> bool {
        code == ERROR_BROKEN_PIPE as u32
            || code == ERROR_PIPE_NOT_CONNECTED as u32
            || code == ERROR_NO_DATA as u32
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn pipe_path(endpoint: &str) -> String {
        if endpoint.starts_with(r"\\") {
            endpoint.to_string()
        } else {
            format!(r"\\.\pipe\{endpoint}")
        }
    }

    struct PipeHandle(HANDLE);

    // HANDLE is a kernel object handle; all I/O is synchronous and guarded by
    // `&self`, so sharing it across the reader and writer is safe.
    unsafe impl Send for PipeHandle {}
    unsafe impl Sync for PipeHandle {}

    impl Drop for PipeHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    impl PipeHandle {
        fn read_some(&self, buffer: &mut [u8]) -> Result<usize, SdkError> {
            let mut read = 0u32;
            let ok = unsafe {
                ReadFile(
                    self.0,
                    buffer.as_mut_ptr() as *mut c_void,
                    buffer.len() as u32,
                    &mut read,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_error("ReadFile"));
            }
            Ok(read as usize)
        }

        fn write_all(&self, data: &[u8]) -> Result<(), SdkError> {
            let mut written = 0usize;
            while written < data.len() {
                let chunk = &data[written..];
                let mut count = 0u32;
                let ok = unsafe {
                    WriteFile(
                        self.0,
                        chunk.as_ptr() as *const c_void,
                        chunk.len() as u32,
                        &mut count,
                        null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(last_error("WriteFile"));
                }
                if count == 0 {
                    return Err(SdkError::Io("WriteFile wrote zero bytes".into()));
                }
                written += count as usize;
            }
            Ok(())
        }

        fn available(&self) -> Result<u32, SdkError> {
            let mut avail = 0u32;
            let ok =
                unsafe { PeekNamedPipe(self.0, null_mut(), 0, null_mut(), &mut avail, null_mut()) };
            if ok == 0 {
                return Err(last_error("PeekNamedPipe"));
            }
            Ok(avail)
        }
    }

    struct PipeReader {
        handle: Arc<PipeHandle>,
        buffer: Vec<u8>,
        stop: Arc<AtomicBool>,
    }

    impl FrameReader for PipeReader {
        fn read_frame(&mut self) -> Result<Option<Vec<u8>>, SdkError> {
            loop {
                if let Some(frame) = take_frame(&mut self.buffer)? {
                    if frame.is_empty() {
                        continue;
                    }
                    return Ok(Some(frame));
                }
                if self.stop.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let available = match self.handle.available() {
                    Ok(available) => available,
                    Err(SdkError::Win32 { code, .. }) if is_closed(code) => return Ok(None),
                    Err(error) => return Err(error),
                };
                if available == 0 {
                    thread::sleep(POLL_INTERVAL);
                    continue;
                }
                let mut chunk = [0u8; 8192];
                let room = (MAX_FRAME_SIZE + 1).saturating_sub(self.buffer.len());
                let take = chunk.len().min(room).max(1);
                match self.handle.read_some(&mut chunk[..take]) {
                    Ok(0) => return Ok(None),
                    Ok(count) => self.buffer.extend_from_slice(&chunk[..count]),
                    Err(SdkError::Win32 { code, .. }) if is_closed(code) => return Ok(None),
                    Err(error) => return Err(error),
                }
            }
        }
    }

    struct PipeWriter {
        handle: Arc<PipeHandle>,
    }

    impl FrameWriter for PipeWriter {
        fn write_frame(&mut self, frame: &[u8]) -> Result<(), SdkError> {
            self.handle.write_all(&framed_bytes(frame))
        }
    }

    fn connection(handle: HANDLE, stop: Arc<AtomicBool>) -> Connection {
        let handle = Arc::new(PipeHandle(handle));
        Connection {
            reader: Box::new(PipeReader {
                handle: Arc::clone(&handle),
                buffer: Vec::new(),
                stop: Arc::clone(&stop),
            }),
            writer: Arc::new(Mutex::new(Box::new(PipeWriter { handle }))),
            stop,
            peer_uid: None,
        }
    }

    pub fn connect(endpoint: &str, timeout: Duration) -> Result<Connection, SdkError> {
        let path = to_wide(&pipe_path(endpoint));
        let deadline = Instant::now() + timeout;
        loop {
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE as u32,
                    0,
                    null(),
                    OPEN_EXISTING as u32,
                    FILE_ATTRIBUTE_NORMAL as u32,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(connection(handle, Arc::new(AtomicBool::new(false))));
            }
            let code = unsafe { GetLastError() };
            match code {
                code if code == ERROR_ACCESS_DENIED as u32 => {
                    return Err(SdkError::PermissionDenied)
                }
                code if code == ERROR_PIPE_BUSY as u32 => unsafe {
                    WaitNamedPipeW(path.as_ptr(), 100);
                },
                code if code == ERROR_FILE_NOT_FOUND as u32 => {
                    if Instant::now() >= deadline {
                        return Err(SdkError::DaemonUnavailable);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                _ => return Err(win32("CreateFileW", code)),
            }
            if Instant::now() >= deadline {
                return Err(SdkError::DaemonUnavailable);
            }
        }
    }

    /// DACL granting full control to SYSTEM, Administrators and the current
    /// user only. Remote clients are rejected by the pipe itself.
    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

    unsafe impl Send for SecurityDescriptor {}
    unsafe impl Sync for SecurityDescriptor {}

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    fn wide_ptr_to_string(ptr: *const u16) -> String {
        let mut len = 0usize;
        unsafe {
            while *ptr.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
        }
    }

    fn current_user_sid() -> Result<String, SdkError> {
        let mut token: HANDLE = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY as u32, &mut token) } == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        let result = (|| {
            let mut size = 0u32;
            unsafe {
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut size);
            }
            if size == 0 {
                return Err(last_error("GetTokenInformation"));
            }
            let mut buffer = vec![0u8; size as usize];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr() as *mut c_void,
                    size,
                    &mut size,
                )
            } == 0
            {
                return Err(last_error("GetTokenInformation"));
            }
            let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
            let mut sid_string: PWSTR = null_mut();
            if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
                return Err(last_error("ConvertSidToStringSidW"));
            }
            let sid = wide_ptr_to_string(sid_string);
            unsafe {
                LocalFree(sid_string as *mut c_void);
            }
            Ok(sid)
        })();
        unsafe {
            CloseHandle(token);
        }
        result
    }

    impl SecurityDescriptor {
        fn allow_current_user() -> Result<Self, SdkError> {
            let sid = current_user_sid()?;
            let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{sid})");
            let wide = to_wide(&sddl);
            let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1 as u32,
                    &mut descriptor,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(last_error(
                    "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                ));
            }
            Ok(Self(descriptor))
        }
    }

    pub struct Listener {
        name: String,
        wide: Vec<u16>,
        security: SecurityDescriptor,
        reject_remote: AtomicBool,
    }

    impl Listener {
        pub fn bind(endpoint: &str) -> Result<Self, SdkError> {
            let name = pipe_path(endpoint);
            Ok(Self {
                wide: to_wide(&name),
                name,
                security: SecurityDescriptor::allow_current_user()?,
                reject_remote: AtomicBool::new(true),
            })
        }

        pub fn endpoint(&self) -> &str {
            &self.name
        }

        fn create(&self, reject_remote: bool) -> Result<HANDLE, SdkError> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.security.0,
                bInheritHandle: 0,
            };
            let mut open_mode = PIPE_ACCESS_DUPLEX as u32;
            if reject_remote {
                open_mode |= PIPE_REJECT_REMOTE_CLIENTS as u32;
            }
            let handle = unsafe {
                CreateNamedPipeW(
                    self.wide.as_ptr(),
                    open_mode,
                    (PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT) as u32,
                    PIPE_INSTANCES,
                    PIPE_BUFFER,
                    PIPE_BUFFER,
                    0,
                    &attributes,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(last_error("CreateNamedPipeW"));
            }
            Ok(handle)
        }

        pub fn accept(&self) -> Result<Connection, SdkError> {
            let reject_remote = self.reject_remote.load(Ordering::Relaxed);
            let handle = match self.create(reject_remote) {
                Ok(handle) => handle,
                // Some Windows builds (observed on 10.0.26220) reject the flag
                // itself with ERROR_INVALID_PARAMETER. The DACL still limits the
                // pipe to SYSTEM, Administrators and the current user.
                Err(SdkError::Win32 { code, .. })
                    if reject_remote && code == ERROR_INVALID_PARAMETER as u32 =>
                {
                    self.reject_remote.store(false, Ordering::Relaxed);
                    self.create(false)?
                }
                Err(error) => return Err(error),
            };
            let connected = unsafe { ConnectNamedPipe(handle, null_mut()) };
            if connected == 0 {
                let code = unsafe { GetLastError() };
                if code != ERROR_PIPE_CONNECTED as u32 {
                    unsafe {
                        CloseHandle(handle);
                    }
                    return Err(win32("ConnectNamedPipe", code));
                }
            }
            Ok(connection(handle, Arc::new(AtomicBool::new(false))))
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use com0com_core::SdkError;

    use crate::framing::{framed_bytes, take_frame, Connection, FrameReader, FrameWriter};
    use crate::MAX_FRAME_SIZE;

    const READ_POLL: Duration = Duration::from_millis(20);

    struct SocketReader {
        stream: UnixStream,
        buffer: Vec<u8>,
        stop: Arc<AtomicBool>,
    }

    impl FrameReader for SocketReader {
        fn read_frame(&mut self) -> Result<Option<Vec<u8>>, SdkError> {
            loop {
                if let Some(frame) = take_frame(&mut self.buffer)? {
                    if frame.is_empty() {
                        continue;
                    }
                    return Ok(Some(frame));
                }
                if self.stop.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let mut chunk = [0u8; 8192];
                let room = (MAX_FRAME_SIZE + 1).saturating_sub(self.buffer.len());
                let take = chunk.len().min(room).max(1);
                match self.stream.read(&mut chunk[..take]) {
                    Ok(0) => return Ok(None),
                    Ok(count) => self.buffer.extend_from_slice(&chunk[..count]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(SdkError::Io(error.to_string())),
                }
            }
        }
    }

    struct SocketWriter {
        stream: UnixStream,
    }

    impl FrameWriter for SocketWriter {
        fn write_frame(&mut self, frame: &[u8]) -> Result<(), SdkError> {
            self.stream
                .write_all(&framed_bytes(frame))
                .map_err(|error| SdkError::Io(error.to_string()))
        }
    }

    fn connection(stream: UnixStream, stop: Arc<AtomicBool>) -> Result<Connection, SdkError> {
        stream
            .set_read_timeout(Some(READ_POLL))
            .map_err(|error| SdkError::Io(error.to_string()))?;
        let peer_uid = peer_uid(&stream);
        let writer = stream
            .try_clone()
            .map_err(|error| SdkError::Io(error.to_string()))?;
        Ok(Connection {
            reader: Box::new(SocketReader {
                stream,
                buffer: Vec::new(),
                stop: Arc::clone(&stop),
            }),
            writer: Arc::new(Mutex::new(Box::new(SocketWriter { stream: writer }))),
            stop,
            peer_uid,
        })
    }

    pub fn connect(endpoint: &str, timeout: Duration) -> Result<Connection, SdkError> {
        let deadline = Instant::now() + timeout;
        loop {
            match UnixStream::connect(endpoint) {
                Ok(stream) => return connection(stream, Arc::new(AtomicBool::new(false))),
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return Err(SdkError::DaemonUnavailable);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                    return Err(SdkError::PermissionDenied)
                }
                Err(error) => return Err(SdkError::Io(error.to_string())),
            }
            if Instant::now() >= deadline {
                return Err(SdkError::DaemonUnavailable);
            }
        }
    }

    pub struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        pub fn bind(endpoint: &str) -> Result<Self, SdkError> {
            let path = PathBuf::from(endpoint);
            if let Ok(metadata) = std::fs::metadata(&path) {
                if !metadata.file_type().is_socket() {
                    return Err(SdkError::Io(format!(
                        "{} exists and is not a socket",
                        path.display()
                    )));
                }
                std::fs::remove_file(&path).map_err(|error| SdkError::Io(error.to_string()))?;
            }
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                // The socket directory is a trust boundary; create it private
                // and never chmod a directory that already existed.
                let created = std::fs::symlink_metadata(parent).is_err();
                std::fs::create_dir_all(parent).map_err(|error| SdkError::Io(error.to_string()))?;
                if created {
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| SdkError::Io(error.to_string()))?;
                }
            }
            let listener = UnixListener::bind(&path)
                .map_err(|error| SdkError::Io(format!("bind {}: {error}", path.display())))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| SdkError::Io(error.to_string()))?;
            Ok(Self { listener, path })
        }

        pub fn endpoint(&self) -> &str {
            self.path.to_str().unwrap_or("")
        }

        pub fn accept(&self) -> Result<Connection, SdkError> {
            let (stream, _addr) = self
                .listener
                .accept()
                .map_err(|error| SdkError::Io(error.to_string()))?;
            connection(stream, Arc::new(AtomicBool::new(false)))
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Peer credentials of a connected control socket. The daemon uses this
    /// with its `--allow-uid` list; socket permissions are the first gate.
    pub fn peer_uid(stream: &UnixStream) -> Option<u32> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let mut cred = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            let rc = unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    &mut cred as *mut _ as *mut libc::c_void,
                    &mut len,
                )
            };
            if rc == 0 {
                Some(cred.uid)
            } else {
                None
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::io::AsRawFd;
            let mut uid = 0u32;
            let mut gid = 0u32;
            let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
            if rc == 0 {
                Some(uid)
            } else {
                None
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = stream;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{send_notification, write_json};
    use serde_json::json;

    #[test]
    fn memory_transport_roundtrips_frames_both_ways() {
        let (mut a, mut b) = memory_pair();
        write_json(&a.writer, &json!({"id": 1, "method": "ping"})).unwrap();
        let frame = b.reader.read_frame().unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(value["method"], "ping");

        send_notification(&b.writer, "event", json!({"subscription": 1})).unwrap();
        let frame = a.reader.read_frame().unwrap().unwrap();
        assert!(String::from_utf8(frame).unwrap().contains("\"event\""));
    }

    #[test]
    fn memory_transport_reports_eof_when_the_peer_is_dropped() {
        let (mut a, b) = memory_pair();
        drop(b);
        assert!(a.reader.read_frame().unwrap().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn pipe_name_is_normalized() {
        assert_eq!(windows::pipe_path("com0comd-v1"), r"\\.\pipe\com0comd-v1");
        assert_eq!(windows::pipe_path(r"\\.\pipe\custom"), r"\\.\pipe\custom");
    }

    #[cfg(windows)]
    #[test]
    fn pipe_listener_accepts_a_client() {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let name = format!(
            r"\\.\pipe\com0com-ipc-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let listener = Listener::bind(&name).unwrap();
        assert_eq!(listener.endpoint(), name);
        let accept = std::thread::spawn(move || listener.accept().map(|_| ()));
        let client = connect(Some(&name), std::time::Duration::from_secs(2));
        let accepted = accept.join().unwrap();
        assert!(accepted.is_ok(), "accept failed: {accepted:?}");
        drop(client.unwrap());
    }
}
