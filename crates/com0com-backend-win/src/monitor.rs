//! Serial monitor for the Windows backend.
//!
//! The monitor opens the COM port with `FILE_FLAG_OVERLAPPED` and owns a reader
//! thread. Data is delivered through a bounded queue: when the consumer falls
//! behind the reader waits instead of discarding bytes, so `dropped()` only
//! counts items that could not be queued at all (never normal data).

#[cfg(any(windows, test))]
use std::time::Duration;

use com0com_core::{MonitorHandle, PortConfig, PortName, SdkError};

/// Bounded read queue, matching the other backends.
#[cfg(windows)]
const MONITOR_QUEUE: usize = 256;
/// Read chunk size.
#[cfg(windows)]
const READ_BUFFER: usize = 4096;

/// Build the Win32 device path for a port label.
#[cfg(any(windows, test))]
pub(crate) fn device_path(name: &str) -> String {
    if name.starts_with(r"\\.\") || name.starts_with(r"\\?\") || name.contains('\\') {
        name.to_string()
    } else {
        format!(r"\\.\{name}")
    }
}

#[cfg(any(windows, test))]
fn duration_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

pub(crate) fn open(port: &PortName, cfg: &PortConfig) -> Result<Box<dyn MonitorHandle>, SdkError> {
    cfg.validate()?;
    if port.is_empty() {
        return Err(SdkError::InvalidArgument(
            "monitor requires a non-empty port name".into(),
        ));
    }
    #[cfg(windows)]
    {
        windows::open(port, cfg)
    }
    #[cfg(not(windows))]
    {
        Err(SdkError::Unsupported)
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::mem::zeroed;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, SyncSender, TrySendError};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use windows_sys::Win32::errhandlingapi::GetLastError;
    use windows_sys::Win32::fileapi::{CreateFileW, ReadFile, WriteFile, OPEN_EXISTING};
    use windows_sys::Win32::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::ioapiset::{CancelIoEx, GetOverlappedResult};
    use windows_sys::Win32::minwinbase::OVERLAPPED;
    use windows_sys::Win32::synchapi::{CreateEventW, ResetEvent, WaitForSingleObject};
    use windows_sys::Win32::winbase::{
        GetCommState, SetCommState, SetCommTimeouts, COMMTIMEOUTS, DCB, DTR_CONTROL_ENABLE,
        DTR_CONTROL_HANDSHAKE, EVENPARITY, FILE_FLAG_OVERLAPPED, MARKPARITY, NOPARITY, ODDPARITY,
        ONE5STOPBITS, ONESTOPBIT, RTS_CONTROL_ENABLE, RTS_CONTROL_HANDSHAKE, SPACEPARITY,
        TWOSTOPBITS, WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::winerror::{ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, WAIT_TIMEOUT};
    use windows_sys::Win32::winnt::{FILE_ATTRIBUTE_NORMAL, GENERIC_READ, GENERIC_WRITE, HANDLE};

    use com0com_core::{
        FlowControl, MonitorEvent, MonitorHandle, Parity, PortConfig, PortName, SdkError, StopBits,
    };

    use super::{device_path, duration_ms, MONITOR_QUEUE, READ_BUFFER};

    struct SerialMonitor {
        handle: usize,
        receiver: mpsc::Receiver<MonitorEvent>,
        dropped: Arc<AtomicU64>,
        closed: Arc<AtomicBool>,
        reader: Option<JoinHandle<()>>,
        write_timeout: Option<Duration>,
    }

    impl SerialMonitor {
        fn handle(&self) -> HANDLE {
            self.handle as HANDLE
        }
    }

    impl MonitorHandle for SerialMonitor {
        fn write(&mut self, data: &[u8]) -> Result<usize, SdkError> {
            if data.is_empty() {
                return Ok(0);
            }
            let handle = self.handle();
            // SAFETY: manual-reset event, no name, no security attributes.
            let event = unsafe { CreateEventW(null(), 1, 0, null()) };
            if event.is_null() {
                return Err(win32_error("CreateEventW"));
            }
            let mut overlapped: OVERLAPPED = unsafe { zeroed() };
            overlapped.hEvent = event;
            let mut written: u32 = 0;
            // SAFETY: the buffer is live for the call and `overlapped` owns the
            // completion event.
            let ok = unsafe {
                WriteFile(
                    handle,
                    data.as_ptr() as *const c_void,
                    data.len() as u32,
                    &mut written,
                    &mut overlapped,
                )
            };
            let result = if ok != 0 {
                Ok(written as usize)
            } else {
                let code = unsafe { GetLastError() };
                if code != ERROR_IO_PENDING as u32 {
                    Err(win32_error("WriteFile"))
                } else {
                    self.finish_write(handle, &overlapped, event)
                }
            };
            // SAFETY: the event is owned here and the overlapped operation has
            // completed (successfully, failed or was cancelled) before this.
            unsafe { CloseHandle(event) };
            result
        }

        fn events(&self) -> &mpsc::Receiver<MonitorEvent> {
            &self.receiver
        }

        fn dropped(&self) -> u64 {
            self.dropped.load(Ordering::Relaxed)
        }
    }

    impl SerialMonitor {
        fn finish_write(
            &self,
            handle: HANDLE,
            overlapped: &OVERLAPPED,
            event: HANDLE,
        ) -> Result<usize, SdkError> {
            let wait = unsafe {
                WaitForSingleObject(
                    event,
                    self.write_timeout.map(duration_ms).unwrap_or(u32::MAX),
                )
            };
            if wait == WAIT_OBJECT_0 as u32 {
                let mut transferred: u32 = 0;
                // SAFETY: the operation has completed, so a non-blocking query
                // is enough.
                let ok = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, 0) };
                return if ok != 0 {
                    Ok(transferred as usize)
                } else {
                    Err(win32_error("GetOverlappedResult"))
                };
            }
            if wait == WAIT_TIMEOUT as u32 {
                // SAFETY: cancel the pending write; the drain below waits for
                // the final completion so the handle stays valid.
                unsafe { CancelIoEx(handle, overlapped) };
                let mut transferred: u32 = 0;
                let _ = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, 1) };
                return Err(SdkError::Timeout);
            }
            Err(win32_error("WaitForSingleObject"))
        }
    }

    impl Drop for SerialMonitor {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::Release);
            // SAFETY: cancels every pending I/O on this handle; the reader
            // checks `closed` and its own `CancelIoEx` covers the race where the
            // read had not been issued yet.
            unsafe { CancelIoEx(self.handle(), null_mut()) };
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            // SAFETY: the handle came from `CreateFileW`, the reader is joined
            // and no I/O remains.
            unsafe { CloseHandle(self.handle()) };
        }
    }

    pub(super) fn open(
        port: &PortName,
        cfg: &PortConfig,
    ) -> Result<Box<dyn MonitorHandle>, SdkError> {
        let path = device_path(port.as_str());
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        // SAFETY: NUL-terminated path; exclusive share mode because two
        // readers on one serial port cannot be interleaved safely.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE as u32,
                0,
                null(),
                OPEN_EXISTING as u32,
                (FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED) as u32,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = unsafe { GetLastError() };
            return Err(match code {
                // The port is already open elsewhere, or access is denied.
                5 => SdkError::Busy,
                2 | 3 | 1168 => SdkError::NotFound,
                _ => SdkError::Win32 {
                    op: "CreateFileW".into(),
                    code,
                    hint: Some(format!("Could not open {path}")),
                },
            });
        }

        if let Err(error) = configure(handle, cfg) {
            // SAFETY: the handle is owned here and not shared yet.
            unsafe { CloseHandle(handle) };
            return Err(error);
        }

        let (sender, receiver) = mpsc::sync_channel(MONITOR_QUEUE);
        let closed = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let thread_closed = Arc::clone(&closed);
        let thread_dropped = Arc::clone(&dropped);
        let reader_handle = handle as usize;
        let reader = thread::Builder::new()
            .name(format!("com0com-monitor-{}", port.as_str()))
            .spawn(move || {
                read_loop(
                    reader_handle as HANDLE,
                    sender,
                    thread_closed,
                    thread_dropped,
                )
            })
            .map_err(|error| {
                // SAFETY: no reader exists, the handle is still owned here.
                unsafe { CloseHandle(handle) };
                SdkError::Io(format!("could not start monitor thread: {error}"))
            })?;

        Ok(Box::new(SerialMonitor {
            handle: handle as usize,
            receiver,
            dropped,
            closed,
            reader: Some(reader),
            write_timeout: cfg.write_timeout,
        }))
    }

    fn configure(handle: HANDLE, cfg: &PortConfig) -> Result<(), SdkError> {
        let mut dcb: DCB = unsafe { zeroed() };
        dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
        // SAFETY: DCB is fully initialised with the documented length.
        if unsafe { GetCommState(handle, &mut dcb) } == 0 {
            return Err(win32_error("GetCommState"));
        }

        dcb.BaudRate = cfg.baud_rate;
        dcb.ByteSize = cfg.data_bits;
        dcb.Parity = match cfg.parity {
            Parity::None => NOPARITY as u8,
            Parity::Odd => ODDPARITY as u8,
            Parity::Even => EVENPARITY as u8,
            Parity::Mark => MARKPARITY as u8,
            Parity::Space => SPACEPARITY as u8,
        };
        dcb.StopBits = match cfg.stop_bits {
            StopBits::One => ONESTOPBIT as u8,
            StopBits::OnePointFive => ONE5STOPBITS as u8,
            StopBits::Two => TWOSTOPBITS as u8,
        };

        // Clear fParity, fOutxCtsFlow, fOutxDsrFlow, fDtrControl, fDsrSensitivity,
        // fOutX, fInX and fRtsControl before applying the requested flow mode.
        const FLOW_MASK: u32 =
            (1 << 1) | (1 << 2) | (1 << 3) | (3 << 4) | (1 << 6) | (1 << 8) | (1 << 9) | (3 << 12);
        let mut flags = (dcb._bitfield & !FLOW_MASK) | 1; // fBinary is mandatory.
        if cfg.parity != Parity::None {
            flags |= 1 << 1;
        }
        let (dtr, rts) = match cfg.flow_control {
            FlowControl::None => (DTR_CONTROL_ENABLE as u32, RTS_CONTROL_ENABLE as u32),
            FlowControl::RtsCts => {
                flags |= 1 << 2;
                (DTR_CONTROL_ENABLE as u32, RTS_CONTROL_HANDSHAKE as u32)
            }
            FlowControl::XonXoff => {
                flags |= (1 << 8) | (1 << 9);
                (DTR_CONTROL_ENABLE as u32, RTS_CONTROL_ENABLE as u32)
            }
            FlowControl::DtrDsr => {
                flags |= (1 << 3) | (1 << 6);
                (DTR_CONTROL_HANDSHAKE as u32, RTS_CONTROL_ENABLE as u32)
            }
        };
        dcb._bitfield = flags | (dtr << 4) | (rts << 12);
        // SAFETY: DCB is fully populated.
        if unsafe { SetCommState(handle, &dcb) } == 0 {
            return Err(win32_error("SetCommState"));
        }

        let mut timeouts: COMMTIMEOUTS = unsafe { zeroed() };
        if let Some(read_timeout) = cfg.read_timeout {
            // MAXDWORD/MAXDWORD/constant: return as soon as bytes are available,
            // otherwise wait at most `constant` milliseconds.
            timeouts.ReadIntervalTimeout = u32::MAX;
            timeouts.ReadTotalTimeoutMultiplier = u32::MAX;
            timeouts.ReadTotalTimeoutConstant = duration_ms(read_timeout).min(u32::MAX - 1);
        }
        if let Some(write_timeout) = cfg.write_timeout {
            // Enforced by `finish_write`; keep the buffered values coherent.
            timeouts.WriteTotalTimeoutConstant = duration_ms(write_timeout);
        }
        // SAFETY: COMMTIMEOUTS is fully initialised.
        if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
            return Err(win32_error("SetCommTimeouts"));
        }
        Ok(())
    }

    fn read_loop(
        handle: HANDLE,
        sender: SyncSender<MonitorEvent>,
        closed: Arc<AtomicBool>,
        dropped: Arc<AtomicU64>,
    ) {
        // SAFETY: manual-reset event, no name, no security attributes.
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            let error = win32_error("CreateEventW");
            deliver(
                &sender,
                &closed,
                &dropped,
                MonitorEvent::Error(error.to_string()),
            );
            let _ = sender.try_send(MonitorEvent::Closed);
            return;
        }
        let mut buffer = vec![0u8; READ_BUFFER];
        loop {
            if closed.load(Ordering::Acquire) {
                break;
            }
            let mut overlapped: OVERLAPPED = unsafe { zeroed() };
            overlapped.hEvent = event;
            // SAFETY: reset the manual-reset event for this operation.
            unsafe { ResetEvent(event) };
            let mut read: u32 = 0;
            // SAFETY: `buffer` and `overlapped` live for the call.
            let ok = unsafe {
                ReadFile(
                    handle,
                    buffer.as_mut_ptr() as *mut c_void,
                    buffer.len() as u32,
                    &mut read,
                    &mut overlapped,
                )
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_IO_PENDING as u32 {
                    if let Some(bytes) = wait_for_read(handle, event, &overlapped, &closed) {
                        read = bytes;
                    } else {
                        break;
                    }
                } else if code == ERROR_OPERATION_ABORTED as u32 || closed.load(Ordering::Acquire) {
                    break;
                } else {
                    let error = win32_error("ReadFile");
                    deliver(
                        &sender,
                        &closed,
                        &dropped,
                        MonitorEvent::Error(error.to_string()),
                    );
                    break;
                }
            }
            if read > 0
                && !deliver(
                    &sender,
                    &closed,
                    &dropped,
                    MonitorEvent::Data(buffer[..read as usize].to_vec()),
                )
            {
                break;
            }
        }
        // SAFETY: no read refers to the event any more.
        unsafe { CloseHandle(event) };
        if !closed.load(Ordering::Acquire) {
            let _ = deliver(&sender, &closed, &dropped, MonitorEvent::Closed);
        }
    }

    /// Wait for a pending read, returning `Some(bytes)` on success. `None` means
    /// the monitor is shutting down or the device disappeared; an error event is
    /// queued for the latter.
    fn wait_for_read(
        handle: HANDLE,
        event: HANDLE,
        overlapped: &OVERLAPPED,
        closed: &AtomicBool,
    ) -> Option<u32> {
        loop {
            // SAFETY: `event` belongs to the pending operation.
            let wait = unsafe { WaitForSingleObject(event, 50) };
            if wait == WAIT_OBJECT_0 as u32 {
                break;
            }
            if wait == WAIT_FAILED {
                // SAFETY: cancel a read whose event cannot be waited on.
                unsafe { CancelIoEx(handle, overlapped) };
                let mut drained: u32 = 0;
                let _ = unsafe { GetOverlappedResult(handle, overlapped, &mut drained, 1) };
                return None;
            }
            if closed.load(Ordering::Acquire) {
                // SAFETY: cancels the outstanding read so the drain below does
                // not block forever.
                unsafe { CancelIoEx(handle, overlapped) };
            }
        }
        let mut transferred: u32 = 0;
        // SAFETY: the operation completed (the event is signalled).
        let ok = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, 1) };
        if ok != 0 {
            return Some(transferred);
        }
        let code = unsafe { GetLastError() };
        if code == ERROR_OPERATION_ABORTED as u32 || closed.load(Ordering::Acquire) {
            return None;
        }
        None
    }

    /// Queue one event, retrying while the bounded queue is full. This is the
    /// backpressure path: the reader stalls instead of dropping bytes.
    fn deliver(
        sender: &SyncSender<MonitorEvent>,
        closed: &AtomicBool,
        dropped: &AtomicU64,
        mut event: MonitorEvent,
    ) -> bool {
        loop {
            match sender.try_send(event) {
                Ok(()) => return true,
                Err(TrySendError::Full(returned)) => {
                    if closed.load(Ordering::Acquire) {
                        dropped.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                    event = returned;
                    thread::sleep(Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }

    fn win32_error(op: &'static str) -> SdkError {
        SdkError::Win32 {
            op: op.into(),
            code: unsafe { GetLastError() },
            hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_path_wraps_plain_com_names() {
        assert_eq!(device_path("COM15"), r"\\.\COM15");
        assert_eq!(device_path(r"\\.\COM7"), r"\\.\COM7");
        assert_eq!(device_path(r"\\?\usb#vid"), r"\\?\usb#vid");
    }

    #[test]
    fn duration_conversion_clamps_to_u32() {
        assert_eq!(duration_ms(Duration::from_millis(1500)), 1500);
        assert_eq!(
            duration_ms(Duration::from_secs(u64::from(u32::MAX))),
            u32::MAX
        );
    }
}
