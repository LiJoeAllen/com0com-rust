//! Serial monitor on top of a PTY slave.
//!
//! `open_monitor` validates the port against the managed list in the backend,
//! so callers cannot ask the daemon to open arbitrary files. Reads happen on a
//! dedicated thread and are delivered through a bounded queue; a slow consumer
//! applies backpressure to the PTY instead of silently losing bytes.

use std::io;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use com0com_core::{
    FlowControl, MonitorEvent, MonitorHandle, Parity, PortConfig, SdkError, StopBits,
};

use crate::pty::{configure_raw, open_path};

const MONITOR_QUEUE: usize = 256;
const POLL_TIMEOUT_MS: i32 = 100;

pub(crate) struct PtyMonitor {
    fd: RawFd,
    receiver: Receiver<MonitorEvent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
    write_timeout: Duration,
}

pub(crate) fn open(target: &Path, cfg: &PortConfig) -> Result<PtyMonitor, SdkError> {
    cfg.validate()?;
    let fd = open_path(target, libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK)?;
    if let Err(error) = configure_raw(fd).and_then(|()| apply_config(fd, cfg)) {
        unsafe { libc::close(fd) };
        return Err(error);
    }

    let (sender, receiver) = mpsc::sync_channel(MONITOR_QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let thread = match thread::Builder::new()
        .name("com0com-pty-monitor".into())
        .spawn({
            let stop = Arc::clone(&stop);
            move || read_loop(fd, sender, stop)
        }) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe { libc::close(fd) };
            return Err(SdkError::Io(format!("spawn monitor thread: {error}")));
        }
    };

    Ok(PtyMonitor {
        fd,
        receiver,
        stop,
        thread: Some(thread),
        dropped,
        write_timeout: cfg.write_timeout.unwrap_or_else(|| Duration::from_secs(1)),
    })
}

impl MonitorHandle for PtyMonitor {
    fn write(&mut self, data: &[u8]) -> Result<usize, SdkError> {
        let deadline = Instant::now() + self.write_timeout;
        let mut offset = 0usize;
        while offset < data.len() {
            let count = unsafe {
                libc::write(
                    self.fd,
                    data[offset..].as_ptr() as *const libc::c_void,
                    data.len() - offset,
                )
            };
            if count > 0 {
                offset += count as usize;
                continue;
            }
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(SdkError::Io(format!("monitor write: {error}")));
                }
            }
            if Instant::now() >= deadline {
                return Err(SdkError::Timeout);
            }
            let mut poll_fd = [libc::pollfd {
                fd: self.fd,
                events: libc::POLLOUT,
                revents: 0,
            }];
            unsafe { libc::poll(poll_fd.as_mut_ptr(), 1, POLL_TIMEOUT_MS) };
        }
        Ok(offset)
    }

    fn events(&self) -> &Receiver<MonitorEvent> {
        &self.receiver
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for PtyMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe { libc::close(self.fd) };
    }
}

fn read_loop(fd: RawFd, sender: SyncSender<MonitorEvent>, stop: Arc<AtomicBool>) {
    let mut buffer = [0u8; 4096];
    let mut poll_fd = [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    while !stop.load(Ordering::Relaxed) {
        let ready = unsafe { libc::poll(poll_fd.as_mut_ptr(), 1, POLL_TIMEOUT_MS) };
        if ready <= 0 || poll_fd[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
            continue;
        }
        let count =
            unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len()) };
        if count > 0 {
            let chunk = buffer[..count as usize].to_vec();
            if sender.send(MonitorEvent::Data(chunk)).is_err() {
                return;
            }
            continue;
        }
        if count == 0 {
            let _ = sender.send(MonitorEvent::Closed);
            return;
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => {}
            io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof => {
                let _ = sender.send(MonitorEvent::Closed);
                return;
            }
            _ => {
                let _ = sender.send(MonitorEvent::Error(error.to_string()));
                let _ = sender.send(MonitorEvent::Closed);
                return;
            }
        }
    }
}

fn apply_config(fd: RawFd, cfg: &PortConfig) -> Result<(), SdkError> {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
        return Err(io_error("tcgetattr"));
    }

    termios.c_cflag &= !libc::CSIZE;
    termios.c_cflag |= match cfg.data_bits {
        5 => libc::CS5,
        6 => libc::CS6,
        7 => libc::CS7,
        _ => libc::CS8,
    };
    match cfg.parity {
        Parity::None => termios.c_cflag &= !(libc::PARENB | libc::PARODD),
        Parity::Odd => termios.c_cflag |= libc::PARENB | libc::PARODD,
        Parity::Even => {
            termios.c_cflag |= libc::PARENB;
            termios.c_cflag &= !libc::PARODD;
        }
        // POSIX has no portable mark/space parity; leave the line as configured.
        Parity::Mark | Parity::Space => {}
    }
    if matches!(cfg.stop_bits, StopBits::Two) {
        termios.c_cflag |= libc::CSTOPB;
    } else {
        termios.c_cflag &= !libc::CSTOPB;
    }
    // `RtsCts`/`DtrDsr` need platform-specific flag names and real modem lines;
    // the PTY backend declares no capability for them, so the flags stay off.
    if matches!(cfg.flow_control, FlowControl::None | FlowControl::XonXoff) {
        #[cfg(target_os = "linux")]
        {
            termios.c_cflag &= !libc::CRTSCTS;
        }
    }

    if let Some(speed) = baud_to_speed(cfg.baud_rate) {
        if unsafe { libc::cfsetispeed(&mut termios, speed) } != 0
            || unsafe { libc::cfsetospeed(&mut termios, speed) } != 0
        {
            return Err(io_error("cfsetospeed"));
        }
    }

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
        return Err(io_error("tcsetattr"));
    }
    Ok(())
}

fn baud_to_speed(baud: u32) -> Option<libc::speed_t> {
    Some(match baud {
        50 => libc::B50,
        75 => libc::B75,
        110 => libc::B110,
        134 => libc::B134,
        150 => libc::B150,
        200 => libc::B200,
        300 => libc::B300,
        600 => libc::B600,
        1200 => libc::B1200,
        1800 => libc::B1800,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19_200 => libc::B19200,
        38_400 => libc::B38400,
        57_600 => libc::B57600,
        115_200 => libc::B115200,
        230_400 => libc::B230400,
        #[cfg(target_os = "linux")]
        460_800 => libc::B460800,
        #[cfg(target_os = "linux")]
        500_000 => libc::B500000,
        #[cfg(target_os = "linux")]
        576_000 => libc::B576000,
        #[cfg(target_os = "linux")]
        921_600 => libc::B921600,
        #[cfg(target_os = "linux")]
        1_000_000 => libc::B1000000,
        _ => return None,
    })
}

fn io_error(op: &str) -> SdkError {
    SdkError::Io(format!("{op}: {}", io::Error::last_os_error()))
}
