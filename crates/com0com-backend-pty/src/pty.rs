//! PTY pair lifecycle and the bidirectional pump.
//!
//! A pair owns two PTYs. Applications open the slave side (through the stable
//! link); the pump copies bytes between the two masters. A slave holder fd is
//! kept open so the master never sees a hangup when no application is
//! connected, and it is also the handle used to read the terminal settings the
//! application configured.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use com0com_core::{PairParams, SdkError};

use crate::links::{create_link, remove_link, LinkConfig};

const POLL_TIMEOUT_MS: i32 = 100;
const READ_CHUNK: usize = 4096;
/// How long a blocked write waits before dropping bytes when `overrun` is on.
const OVERRUN_DROP_AFTER: Duration = Duration::from_millis(500);
/// Largest slice the pacer will delay for, keeping shutdown within ~1 s at
/// low baud rates.
const PACE_SLICE: usize = 512;

pub(crate) struct PtyPort {
    pub(crate) label: String,
    pub(crate) link: PathBuf,
    pub(crate) alias: Option<PathBuf>,
    pub(crate) target: PathBuf,
    master: RawFd,
    holder: RawFd,
}

impl PtyPort {
    fn create(label: String, links: &LinkConfig) -> Result<Self, SdkError> {
        let (master, target) = open_pty()?;
        match open_slave(&target) {
            Ok(holder) => Ok(Self {
                link: links.link_path(&label),
                alias: links.alias_path(&label),
                target,
                label,
                master,
                holder,
            }),
            Err(error) => {
                unsafe { libc::close(master) };
                Err(error)
            }
        }
    }

    fn create_links(&self) -> Result<(), SdkError> {
        create_link(&self.link, &self.target)?;
        if let Some(alias) = &self.alias {
            create_link(alias, &self.target)?;
        }
        Ok(())
    }

    fn remove_links(&self) {
        remove_link(&self.link, &self.target);
        if let Some(alias) = &self.alias {
            remove_link(alias, &self.target);
        }
    }
}

impl Drop for PtyPort {
    fn drop(&mut self) {
        self.remove_links();
        unsafe {
            libc::close(self.master);
            libc::close(self.holder);
        }
    }
}

pub(crate) struct PtyPair {
    pub(crate) a: PtyPort,
    pub(crate) b: PtyPort,
    pub(crate) params: Arc<Mutex<PairParams>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    #[allow(dead_code)]
    dropped: Arc<AtomicU64>,
}

impl PtyPair {
    pub(crate) fn create(
        id: u8,
        labels: [String; 2],
        params: &PairParams,
        links: &LinkConfig,
    ) -> Result<Self, SdkError> {
        let a = PtyPort::create(labels[0].clone(), links)?;
        let b = PtyPort::create(labels[1].clone(), links)?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(params.clone()));
        let dropped = Arc::new(AtomicU64::new(0));
        let mut pair = Self {
            a,
            b,
            params: Arc::clone(&shared),
            stop: Arc::clone(&stop),
            threads: Vec::with_capacity(2),
            dropped: Arc::clone(&dropped),
        };
        pair.a.create_links()?;
        pair.b.create_links()?;

        let a_to_b = spawn_pump(
            pair.a.master,
            pair.b.master,
            pair.b.holder,
            Arc::clone(&stop),
            Arc::clone(&shared),
            seed(id, 0),
            Arc::clone(&dropped),
        )?;
        let b_to_a = match spawn_pump(
            pair.b.master,
            pair.a.master,
            pair.a.holder,
            Arc::clone(&stop),
            shared,
            seed(id, 1),
            dropped,
        ) {
            Ok(handle) => handle,
            Err(error) => {
                stop.store(true, Ordering::SeqCst);
                let _ = a_to_b.join();
                return Err(error);
            }
        };
        pair.threads.push(a_to_b);
        pair.threads.push(b_to_a);
        Ok(pair)
    }

    /// Swap the stable link names without touching the PTYs.
    pub(crate) fn relabel(
        &mut self,
        labels: [String; 2],
        links: &LinkConfig,
    ) -> Result<(), SdkError> {
        let new_links = [links.link_path(&labels[0]), links.link_path(&labels[1])];
        let new_aliases = [links.alias_path(&labels[0]), links.alias_path(&labels[1])];
        let old_links = [self.a.link.clone(), self.b.link.clone()];
        let old_aliases = [self.a.alias.clone(), self.b.alias.clone()];

        // Remove first so a swap (A<->B) cannot collide with the old names.
        self.a.remove_links();
        self.b.remove_links();
        let result = (|| -> Result<(), SdkError> {
            create_link(&new_links[0], &self.a.target)?;
            if let Some(alias) = &new_aliases[0] {
                create_link(alias, &self.a.target)?;
            }
            create_link(&new_links[1], &self.b.target)?;
            if let Some(alias) = &new_aliases[1] {
                create_link(alias, &self.b.target)?;
            }
            Ok(())
        })();

        if let Err(error) = result {
            // Only unlink symlinks that still point at this port: a regular
            // file that blocked creation must survive the rollback.
            remove_link(&new_links[0], &self.a.target);
            remove_link(&new_links[1], &self.b.target);
            if let Some(alias) = &new_aliases[0] {
                remove_link(alias, &self.a.target);
            }
            if let Some(alias) = &new_aliases[1] {
                remove_link(alias, &self.b.target);
            }
            self.a.link = old_links[0].clone();
            self.b.link = old_links[1].clone();
            self.a.alias = old_aliases[0].clone();
            self.b.alias = old_aliases[1].clone();
            let _ = self.a.create_links();
            let _ = self.b.create_links();
            return Err(error);
        }

        self.a.label = labels[0].clone();
        self.b.label = labels[1].clone();
        self.a.link = new_links[0].clone();
        self.b.link = new_links[1].clone();
        self.a.alias = new_aliases[0].clone();
        self.b.alias = new_aliases[1].clone();
        Ok(())
    }

    pub(crate) fn set_shared_params(&self, params: &PairParams) {
        *self
            .params
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = params.clone();
    }
}

impl Drop for PtyPair {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

fn seed(id: u8, index: u8) -> u32 {
    0x5EED_0000 ^ ((id as u32) << 8) ^ index as u32
}

fn open_pty() -> Result<(RawFd, PathBuf), SdkError> {
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK) };
    if master < 0 {
        return Err(io_error("posix_openpt"));
    }
    if unsafe { libc::grantpt(master) } != 0 {
        let error = io_error("grantpt");
        unsafe { libc::close(master) };
        return Err(error);
    }
    if unsafe { libc::unlockpt(master) } != 0 {
        let error = io_error("unlockpt");
        unsafe { libc::close(master) };
        return Err(error);
    }
    match slave_name(master) {
        Ok(name) => {
            let target = PathBuf::from(name);
            maybe_chown(&target);
            Ok((master, target))
        }
        Err(error) => {
            unsafe { libc::close(master) };
            Err(error)
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn slave_name(master: RawFd) -> Result<String, SdkError> {
    let mut buffer = [0 as libc::c_char; 256];
    let rc = unsafe { libc::ptsname_r(master, buffer.as_mut_ptr(), buffer.len()) };
    if rc != 0 {
        return Err(SdkError::Io(format!(
            "ptsname_r: {}",
            io::Error::from_raw_os_error(rc)
        )));
    }
    let name = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Ok(name.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn slave_name(master: RawFd) -> Result<String, SdkError> {
    use std::sync::Mutex;
    // `ptsname` returns a static buffer on macOS; serialize the copy.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let pointer = unsafe { libc::ptsname(master) };
    if pointer.is_null() {
        return Err(io_error("ptsname"));
    }
    let name = unsafe { std::ffi::CStr::from_ptr(pointer) };
    Ok(name.to_string_lossy().into_owned())
}

/// A root daemon hands the slave to the desktop user when requested, so an
/// unprivileged application can open the node.
fn maybe_chown(target: &Path) {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let Ok(uid) = std::env::var("COM0COM_DEVICE_UID").map_err(|_| ()) else {
        return;
    };
    let Ok(uid) = uid.parse::<u32>() else {
        return;
    };
    let gid = std::env::var("COM0COM_DEVICE_GID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(uid);
    let Ok(path) = CString::new(target.as_os_str().as_bytes()) else {
        return;
    };
    unsafe {
        libc::chown(path.as_ptr(), uid, gid);
    }
}

fn open_slave(target: &Path) -> Result<RawFd, SdkError> {
    let fd = open_path(target, libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK)?;
    if let Err(error) = configure_raw(fd) {
        unsafe { libc::close(fd) };
        return Err(error);
    }
    Ok(fd)
}

pub(crate) fn open_path(path: &Path, flags: libc::c_int) -> Result<RawFd, SdkError> {
    let raw = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| SdkError::InvalidArgument("path contains a NUL byte".into()))?;
    let fd = unsafe { libc::open(raw.as_ptr(), flags) };
    if fd < 0 {
        return Err(io_error(&format!("open {}", path.display())));
    }
    Ok(fd)
}

// `VMIN`/`VTIME` are `usize` on Linux but `c_int` on macOS, so the cast is
// needed on one target and redundant on the other.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn configure_raw(fd: RawFd) -> Result<(), SdkError> {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
        return Err(io_error("tcgetattr"));
    }
    unsafe { libc::cfmakeraw(&mut termios) };
    termios.c_cflag |= libc::CLOCAL | libc::CREAD;
    termios.c_cc[libc::VMIN as usize] = 1;
    termios.c_cc[libc::VTIME as usize] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
        return Err(io_error("tcsetattr"));
    }
    Ok(())
}

fn io_error(op: &str) -> SdkError {
    let error = io::Error::last_os_error();
    match error.kind() {
        io::ErrorKind::PermissionDenied => SdkError::PermissionDenied,
        io::ErrorKind::NotFound => SdkError::NotFound,
        _ => SdkError::Io(format!("{op}: {error}")),
    }
}

fn spawn_pump(
    source: RawFd,
    destination: RawFd,
    destination_holder: RawFd,
    stop: Arc<AtomicBool>,
    params: Arc<Mutex<PairParams>>,
    seed: u32,
    dropped: Arc<AtomicU64>,
) -> Result<JoinHandle<()>, SdkError> {
    thread::Builder::new()
        .name("com0com-pty-pump".into())
        .spawn(move || {
            pump(
                source,
                destination,
                destination_holder,
                stop,
                params,
                seed,
                dropped,
            )
        })
        .map_err(|error| SdkError::Io(format!("spawn PTY pump: {error}")))
}

fn pump(
    source: RawFd,
    destination: RawFd,
    destination_holder: RawFd,
    stop: Arc<AtomicBool>,
    params: Arc<Mutex<PairParams>>,
    seed: u32,
    dropped: Arc<AtomicU64>,
) {
    let mut random = Xorshift32::new(seed);
    let mut pacer = Pacer::default();
    let mut buffer = [0u8; READ_CHUNK];
    let mut poll_fd = [libc::pollfd {
        fd: source,
        events: libc::POLLIN,
        revents: 0,
    }];

    while !stop.load(Ordering::Relaxed) {
        let ready = unsafe { libc::poll(poll_fd.as_mut_ptr(), 1, POLL_TIMEOUT_MS) };
        if ready <= 0 || poll_fd[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
            continue;
        }
        let count = unsafe {
            libc::read(
                source,
                buffer.as_mut_ptr() as *mut libc::c_void,
                buffer.len(),
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::EIO)
            {
                continue;
            }
            break;
        }
        let count = count as usize;
        if count == 0 {
            continue;
        }
        let data = &mut buffer[..count];

        let (baud_emu, noise_enabled, probability, overrun) = {
            let params = params
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                params.baud_emu,
                params.noise_enabled,
                params.noise_probability,
                params.overrun,
            )
        };
        if noise_enabled && probability > 0 {
            random.apply(data, probability);
        }
        if baud_emu {
            pacer.pace(destination_holder, data, &stop);
        }
        let written = write_with_overrun(destination, data, &stop, overrun);
        if written < data.len() {
            dropped.fetch_add((data.len() - written) as u64, Ordering::Relaxed);
        }
    }
}

struct Xorshift32 {
    state: u32,
}

impl Xorshift32 {
    fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    fn next(&mut self) -> u32 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 17;
        value ^= value << 5;
        self.state = value;
        value
    }

    /// Flip one random bit per byte with probability `probability / 10000`.
    fn apply(&mut self, data: &mut [u8], probability: u16) {
        if probability >= 10_000 {
            for byte in data.iter_mut() {
                *byte ^= 1 << (self.next() % 8);
            }
            return;
        }
        for byte in data.iter_mut() {
            if self.next() % 10_000 < probability as u32 {
                *byte ^= 1 << (self.next() % 8);
            }
        }
    }
}

#[derive(Default)]
struct Pacer {
    last: Option<Instant>,
    budget: f64,
}

impl Pacer {
    fn pace(&mut self, holder: RawFd, data: &[u8], stop: &AtomicBool) {
        let rate = match output_baud(holder) {
            0 => return,
            rate => rate,
        };
        let bytes_per_second = rate as f64 / 10.0; // 8N1 framing
        let now = Instant::now();
        if let Some(last) = self.last {
            self.budget += now.duration_since(last).as_secs_f64() * bytes_per_second;
        }
        self.last = Some(now);
        // Cap bursts so a long idle period does not let a huge write through.
        self.budget = self.budget.min(bytes_per_second * 0.5);

        let mut remaining = data.len();
        while remaining > 0 {
            let slice = remaining.min(PACE_SLICE);
            let needed = slice as f64;
            if self.budget >= needed {
                self.budget -= needed;
            } else {
                let deficit = (needed - self.budget) / bytes_per_second;
                self.budget = 0.0;
                sleep_until(Instant::now() + Duration::from_secs_f64(deficit), stop);
                self.last = Some(Instant::now());
            }
            remaining -= slice;
        }
    }
}

fn sleep_until(deadline: Instant, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

fn output_baud(fd: RawFd) -> u32 {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
        return 0;
    }
    speed_to_baud(unsafe { libc::cfgetospeed(&termios) })
}

#[allow(clippy::match_same_arms)]
fn speed_to_baud(speed: libc::speed_t) -> u32 {
    match speed {
        libc::B0 => 0,
        libc::B50 => 50,
        libc::B75 => 75,
        libc::B110 => 110,
        libc::B134 => 134,
        libc::B150 => 150,
        libc::B200 => 200,
        libc::B300 => 300,
        libc::B600 => 600,
        libc::B1200 => 1200,
        libc::B1800 => 1800,
        libc::B2400 => 2400,
        libc::B4800 => 4800,
        libc::B9600 => 9600,
        libc::B19200 => 19_200,
        libc::B38400 => 38_400,
        libc::B57600 => 57_600,
        libc::B115200 => 115_200,
        libc::B230400 => 230_400,
        #[cfg(target_os = "linux")]
        libc::B460800 => 460_800,
        #[cfg(target_os = "linux")]
        libc::B500000 => 500_000,
        #[cfg(target_os = "linux")]
        libc::B576000 => 576_000,
        #[cfg(target_os = "linux")]
        libc::B921600 => 921_600,
        #[cfg(target_os = "linux")]
        libc::B1000000 => 1_000_000,
        #[cfg(target_os = "linux")]
        libc::B1152000 => 1_152_000,
        #[cfg(target_os = "linux")]
        libc::B1500000 => 1_500_000,
        #[cfg(target_os = "linux")]
        libc::B2000000 => 2_000_000,
        #[cfg(target_os = "linux")]
        libc::B2500000 => 2_500_000,
        #[cfg(target_os = "linux")]
        libc::B3000000 => 3_000_000,
        #[cfg(target_os = "linux")]
        libc::B3500000 => 3_500_000,
        #[cfg(target_os = "linux")]
        libc::B4000000 => 4_000_000,
        _ => 0,
    }
}

fn write_with_overrun(fd: RawFd, data: &[u8], stop: &AtomicBool, overrun: bool) -> usize {
    let mut offset = 0usize;
    let mut drop_at: Option<Instant> = None;
    while offset < data.len() && !stop.load(Ordering::Relaxed) {
        let count = unsafe {
            libc::write(
                fd,
                data[offset..].as_ptr() as *const libc::c_void,
                data.len() - offset,
            )
        };
        if count > 0 {
            offset += count as usize;
            drop_at = None;
            continue;
        }
        if count == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                if overrun {
                    let deadline =
                        *drop_at.get_or_insert_with(|| Instant::now() + OVERRUN_DROP_AFTER);
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                let mut poll_fd = [libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                }];
                unsafe { libc::poll(poll_fd.as_mut_ptr(), 1, POLL_TIMEOUT_MS) };
            }
            _ => break,
        }
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_deterministic_and_bounded() {
        let mut first = Xorshift32::new(42);
        let mut second = Xorshift32::new(42);
        let mut a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];
        first.apply(&mut a, 1000);
        second.apply(&mut b, 1000);
        assert_eq!(a, b);
        let changed = a.iter().filter(|byte| **byte != 0).count();
        // ~10% of 4096 bytes, with plenty of slack for the PRNG.
        assert!((250..600).contains(&changed), "changed {changed} bytes");

        let mut all = vec![0u8; 10];
        Xorshift32::new(7).apply(&mut all, 10_000);
        assert!(all.iter().all(|byte| *byte != 0));
    }

    fn set_output_speed(fd: RawFd, speed: libc::speed_t) {
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut termios) }, 0);
        assert_eq!(unsafe { libc::cfsetispeed(&mut termios, speed) }, 0);
        assert_eq!(unsafe { libc::cfsetospeed(&mut termios, speed) }, 0);
        assert_eq!(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) }, 0);
    }

    #[test]
    fn pacer_accepts_large_speeds_immediately() {
        // A fresh PTY defaults to B9600 (macOS does not inherit the caller's
        // setting), which would make 1 KiB take about a second. Pin a fast
        // speed so the assertion measures the pacer, not the default.
        // B38400 is the highest speed musl's cfsetispeed accepts.
        const FAST: libc::speed_t = libc::B38400;

        let mut pacer = Pacer::default();
        // Keep the master open: once it closes, Linux terms the slave hung up
        // and tcgetattr returns EIO, which would silently disable pacing.
        let (master, target) = open_pty().unwrap();
        let holder = open_slave(&target).unwrap();
        set_output_speed(holder, FAST);
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        pacer.pace(holder, &[0u8; 1024], &stop);
        // At B38400 this takes ~267 ms; at the B9600 default it would be ~1.07 s.
        assert!(started.elapsed() < Duration::from_millis(600));
        unsafe {
            libc::close(holder);
            libc::close(master);
        }
    }

    #[test]
    fn pair_transfers_bytes_between_the_two_slaves() {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicU32;

        use crate::links::LinkStyle;

        static NEXT: AtomicU32 = AtomicU32::new(1);
        let dir = std::env::temp_dir().join(format!(
            "com0com-pty-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let links = LinkConfig::at(&dir, LinkStyle::Linux, 0o700).unwrap();
        let params = PairParams::default();
        let pair = PtyPair::create(9, ["A9".into(), "B9".into()], &params, &links).unwrap();

        let mut a = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pair.a.link)
            .unwrap();
        let mut b = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pair.b.link)
            .unwrap();
        a.write_all(b"hello").unwrap();
        let mut buffer = [0u8; 5];
        b.read_exact(&mut buffer).unwrap();
        assert_eq!(&buffer, b"hello");

        drop(pair);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
