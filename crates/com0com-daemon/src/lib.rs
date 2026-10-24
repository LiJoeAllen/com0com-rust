//! `com0comd` server core: single-instance guard, accept loop, restore and
//! autosave. The binary in `main.rs` is a thin CLI over this API so tests can
//! run the real transports against an in-memory backend.

mod selftest;
mod singleton;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use com0com_core::state_io;
use com0com_core::{Backend, LogLevel, RestoreOptions, SdkError};
use com0com_ipc::{serve_connection, DaemonLog, Listener, ServerContext};

use singleton::Singleton;

pub use selftest::selftest;

/// Autosave debounce used by `--autosave` and the system service units.
pub const DEFAULT_AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(2);

const AUTOSAVE_TICK: Duration = Duration::from_millis(100);
const WAKE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct Config {
    /// Pipe name (`com0comd-v1` or a full `\\.\pipe\...` path) or Unix socket
    /// path. Kept as given; [`Listener::endpoint`] returns the normalized form.
    pub endpoint: String,
    pub state_path: PathBuf,
    pub restore: bool,
    pub autosave: bool,
    pub autosave_debounce: Duration,
    pub log_path: Option<PathBuf>,
    /// Unix: uids allowed to talk to the daemon. Empty means the socket mode
    /// (0600) is the only gate. A non-empty list switches the socket to 0660 so
    /// a service group can reach it and each connection's peer cred is checked.
    pub allow_uids: Vec<u32>,
}

impl Config {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            state_path: com0com_core::default_state_path(),
            restore: false,
            autosave: false,
            autosave_debounce: DEFAULT_AUTOSAVE_DEBOUNCE,
            log_path: None,
            allow_uids: Vec::new(),
        }
    }
}

/// A running daemon. Dropping it stops the server and releases the singleton.
pub struct RunningDaemon {
    endpoint: String,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<Result<(), SdkError>>>,
    watcher: Option<JoinHandle<()>>,
    _singleton: Singleton,
}

impl RunningDaemon {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Ask the daemon to stop. [`Self::join`] waits for the accept loop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        wake(&self.endpoint);
    }

    /// Block until the daemon stops on its own (for example after an RPC
    /// `shutdown` request).
    pub fn wait(mut self) -> Result<(), SdkError> {
        let result = self.server.take().map(join_server).unwrap_or(Ok(()));
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        result
    }

    /// Stop and wait for the server thread, including the final autosave.
    pub fn join(self) -> Result<(), SdkError> {
        self.stop();
        self.wait()
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        wake(&self.endpoint);
        if let Some(server) = self.server.take() {
            let _ = join_server(server);
        }
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

fn join_server(handle: JoinHandle<Result<(), SdkError>>) -> Result<(), SdkError> {
    handle
        .join()
        .unwrap_or_else(|_| Err(SdkError::Internal("daemon thread panicked".into())))
}

/// Wake a blocking `accept` with a throwaway client. Failures are ignored:
/// the accept loop also re-checks the stop flag after every timeout/error.
fn wake(endpoint: &str) {
    let _ = com0com_ipc::connect(Some(endpoint), WAKE_TIMEOUT);
}

fn state_io_error(context: &str, error: SdkError) -> SdkError {
    SdkError::Io(format!("{context}: {error}"))
}

/// Start serving. Restore runs synchronously so a corrupt state file fails
/// startup instead of leaving a half-configured daemon.
pub fn spawn(config: Config, backend: Arc<dyn Backend>) -> Result<RunningDaemon, SdkError> {
    let singleton = Singleton::acquire(&config.endpoint)?;
    let listener = Listener::bind(&config.endpoint)?;
    let endpoint = listener.endpoint().to_string();

    if !config.allow_uids.is_empty() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o660)).map_err(
                |error| state_io_error("chmod control socket", SdkError::Io(error.to_string())),
            )?;
        }
    }

    let log = Arc::new(DaemonLog::new(4096, config.log_path.as_deref())?);
    log.push(
        LogLevel::Information,
        format!(
            "com0comd {} listening on {endpoint}",
            env!("CARGO_PKG_VERSION")
        ),
    );
    if config.restore {
        restore_state(&*backend, &config.state_path, &log)?;
    }

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let (autosave_tx, autosave_rx) = mpsc::channel::<()>();
    let mut ctx = ServerContext::new(Arc::clone(&backend), config.state_path.clone())
        .with_log(Arc::clone(&log))
        .with_shutdown(shutdown_tx);
    if config.autosave {
        ctx = ctx.with_autosave(autosave_tx);
    }
    let ctx = Arc::new(ctx);

    let stop = Arc::new(AtomicBool::new(false));
    let autosave = config
        .autosave
        .then(|| {
            Autosave::start(
                Arc::clone(&backend),
                config.state_path.clone(),
                config.autosave_debounce,
                Arc::clone(&log),
                autosave_rx,
                Arc::clone(&stop),
            )
        })
        .transpose()?;
    let watcher = spawn_shutdown_watcher(
        shutdown_rx,
        Arc::clone(&stop),
        endpoint.clone(),
        Arc::clone(&log),
    )?;
    let autosave_enabled = config.autosave;
    let allow_uids = config.allow_uids.clone();
    let server_stop = Arc::clone(&stop);
    let server = thread::Builder::new()
        .name("com0comd-accept".into())
        .spawn(move || {
            serve(
                listener,
                ctx,
                server_stop,
                autosave,
                autosave_enabled,
                allow_uids,
            )
        })
        .map_err(|error| SdkError::Io(format!("spawn accept thread: {error}")))?;

    Ok(RunningDaemon {
        endpoint,
        stop,
        server: Some(server),
        watcher: Some(watcher),
        _singleton: singleton,
    })
}

fn spawn_shutdown_watcher(
    receiver: Receiver<()>,
    stop: Arc<AtomicBool>,
    endpoint: String,
    log: Arc<DaemonLog>,
) -> Result<JoinHandle<()>, SdkError> {
    thread::Builder::new()
        .name("com0comd-shutdown".into())
        .spawn(move || loop {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => {
                    log.push(LogLevel::Information, "shutdown requested");
                    stop.store(true, Ordering::SeqCst);
                    wake(&endpoint);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        })
        .map_err(|error| SdkError::Io(format!("spawn shutdown thread: {error}")))
}

fn serve(
    listener: Listener,
    ctx: Arc<ServerContext>,
    stop: Arc<AtomicBool>,
    autosave: Option<Autosave>,
    autosave_enabled: bool,
    allow_uids: Vec<u32>,
) -> Result<(), SdkError> {
    let live: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
    let next_id = AtomicU64::new(1);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok(connection) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if !peer_allowed(&connection, &allow_uids) {
                    ctx.log().push(
                        LogLevel::Warning,
                        format!(
                            "rejected control connection from uid {:?} (allowed {:?})",
                            connection.peer_uid, allow_uids
                        ),
                    );
                    connection.stop.store(true, Ordering::SeqCst);
                    drop(connection);
                    continue;
                }
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                live.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id, Arc::clone(&connection.stop));
                let ctx_task = Arc::clone(&ctx);
                let live_task = Arc::clone(&live);
                thread::Builder::new()
                    .name("com0comd-connection".into())
                    .spawn(move || {
                        if let Err(error) = serve_connection(Arc::clone(&ctx_task), connection) {
                            ctx_task
                                .log()
                                .push(LogLevel::Warning, format!("connection ended: {error}"));
                        }
                        live_task
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .remove(&id);
                    })
                    .map_err(|error| SdkError::Io(format!("spawn connection thread: {error}")))?;
            }
            Err(error) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                ctx.log()
                    .push(LogLevel::Error, format!("accept failed: {error}"));
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    ctx.log().push(LogLevel::Information, "daemon stopping");
    close_sessions(&live);
    if let Some(worker) = autosave {
        worker.shutdown();
    }
    if autosave_enabled {
        match state_io::save(&**ctx.backend(), ctx.state_path()) {
            Ok(snapshot) => ctx.log().push(
                LogLevel::Information,
                format!("final save wrote {} pair(s)", snapshot.pairs.len()),
            ),
            Err(error) => ctx
                .log()
                .push(LogLevel::Error, format!("final save failed: {error}")),
        }
    }
    Ok(())
}

/// Empty `allow` means the socket's own permissions decide. A configured list
/// must match the Unix peer uid; non-Unix transports have no peer identity but
/// `--allow-uid` is a Unix-only option anyway.
fn peer_allowed(connection: &com0com_ipc::Connection, allow: &[u32]) -> bool {
    if allow.is_empty() {
        return true;
    }
    #[cfg(unix)]
    {
        connection.peer_uid.is_some_and(|uid| allow.contains(&uid))
    }
    #[cfg(not(unix))]
    {
        let _ = connection;
        true
    }
}

/// Make every live session's reader return EOF, then give the handlers a
/// moment to finish so no mutation is in flight during the final save.
fn close_sessions(live: &Mutex<HashMap<u64, Arc<AtomicBool>>>) {
    let handles: Vec<_> = live
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    for handle in handles {
        handle.store(true, Ordering::SeqCst);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !live.lock().unwrap_or_else(|p| p.into_inner()).is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

fn restore_state(backend: &dyn Backend, path: &Path, log: &DaemonLog) -> Result<(), SdkError> {
    if !path.exists() {
        log.push(
            LogLevel::Information,
            format!("restore: {} does not exist yet", path.display()),
        );
        return Ok(());
    }
    let snapshot = state_io::load(path).map_err(|error| state_io_error("restore", error))?;
    let options = RestoreOptions {
        dry_run: false,
        remove_extra: true,
        continue_on_error: true,
    };
    let report = state_io::restore(backend, &snapshot, options)
        .map_err(|error| state_io_error("restore", error))?;
    log.push(
        LogLevel::Information,
        format!(
            "restore applied {} pair(s), {} failure(s)",
            report.applied.len(),
            report.failed.len()
        ),
    );
    for failure in &report.failed {
        log.push(
            LogLevel::Warning,
            format!("restore pair {} failed: {}", failure.pair, failure.error),
        );
    }
    Ok(())
}

/// Select the daemon backend. On Unix the daemon is the process that owns the
/// PTYs; a failure to create them is reported instead of silently serving a
/// stub that cannot do anything.
pub fn native_backend() -> Result<Arc<dyn Backend>, SdkError> {
    #[cfg(windows)]
    {
        Ok(Arc::new(com0com_backend_win::WinBackend::open()?))
    }
    #[cfg(unix)]
    {
        Ok(Arc::new(com0com_backend_pty::PtyBackend::open()?))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(SdkError::Unsupported)
    }
}

struct Autosave {
    stop: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

impl Autosave {
    fn start(
        backend: Arc<dyn Backend>,
        path: PathBuf,
        debounce: Duration,
        log: Arc<DaemonLog>,
        receiver: Receiver<()>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, SdkError> {
        let worker_stop = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("com0comd-autosave".into())
            .spawn(move || {
                let mut deadline: Option<Instant> = None;
                loop {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match receiver.recv_timeout(AUTOSAVE_TICK) {
                        Ok(()) => {
                            deadline.get_or_insert_with(|| Instant::now() + debounce);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    if deadline.is_some_and(|at| Instant::now() >= at) {
                        deadline = None;
                        match state_io::save(&*backend, &path) {
                            Ok(snapshot) => {
                                log.push(
                                    LogLevel::Verbose,
                                    format!("autosaved {} pair(s)", snapshot.pairs.len()),
                                );
                            }
                            Err(error) => {
                                log.push(LogLevel::Error, format!("autosave failed: {error}"))
                            }
                        }
                    }
                }
            })
            .map_err(|error| SdkError::Io(format!("spawn autosave thread: {error}")))?;

        Ok(Self { stop, join })
    }

    fn shutdown(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.join.join();
    }
}
