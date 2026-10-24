//! `com0comd` command line.
//!
//! `run` serves in the foreground (start it from a service manager, Task
//! Scheduler or the UI). `status` and `stop` talk to a running daemon over the
//! same local JSON-RPC endpoint clients use.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use com0com_core::SdkError;
use com0com_daemon::{native_backend, Config};
use com0com_ipc::{Client, RemoteBackend};

const USAGE: &str = "\
com0comd — com0com JSON-RPC daemon

usage:
  com0comd run [--pipe <name>] [--restore] [--autosave] [--log <path>]
               [--allow-uid <uid>]...
  com0comd status [--pipe <name>]
  com0comd stop [--pipe <name>]
  com0comd selftest

  run       serve the local JSON-RPC endpoint in the foreground
  status    report whether a daemon is running
  stop      ask the running daemon to shut down
  selftest  exercise transport + dispatch in-process without a driver

options:
  --pipe <name>    named pipe (Windows) or socket path (Unix) override
  --restore        load the state file into the backend at startup
  --autosave       write the state file after pair changes (and at shutdown)
  --log <path>     append daemon logs to a file
  --allow-uid <uid>  Unix: only accept connections from this uid (repeatable);
                     the socket switches to 0660 so a service group can reach it
";

#[derive(Default)]
struct Options {
    endpoint: Option<String>,
    restore: bool,
    autosave: bool,
    log: Option<PathBuf>,
    allow_uids: Vec<u32>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--pipe" => {
                let value = iter.next().ok_or("--pipe needs a value")?;
                options.endpoint = Some(value.clone());
            }
            "--restore" => options.restore = true,
            "--autosave" => options.autosave = true,
            "--log" => {
                let value = iter.next().ok_or("--log needs a value")?;
                options.log = Some(PathBuf::from(value));
            }
            "--allow-uid" => {
                let value = iter.next().ok_or("--allow-uid needs a value")?;
                let uid = value
                    .parse::<u32>()
                    .map_err(|_| format!("invalid uid: {value}"))?;
                options.allow_uids.push(uid);
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(options)
}

fn endpoint(options: &Options) -> String {
    options
        .endpoint
        .clone()
        .unwrap_or_else(com0com_ipc::default_endpoint)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("status") => status(&args[1..]),
        Some("stop") => stop(&args[1..]),
        Some("selftest") => selftest(&args[1..]),
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}\n\n{USAGE}")),
        None => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("com0comd: {message}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let options = parse_options(args)?;
    let endpoint = endpoint(&options);
    let mut config = Config::new(&endpoint);
    config.restore = options.restore;
    config.autosave = options.autosave;
    config.log_path = options.log;
    config.allow_uids = options.allow_uids;

    let backend = native_backend().map_err(|error| error.to_string())?;
    let daemon = com0com_daemon::spawn(config, backend).map_err(|error| error.to_string())?;
    println!("com0comd listening on {}", daemon.endpoint());
    daemon.wait().map_err(|error| error.to_string())
}

fn status(args: &[String]) -> Result<(), String> {
    let options = parse_options(args)?;
    let endpoint = endpoint(&options);
    match Client::connect_with_timeout(Some(&endpoint), Duration::from_millis(500)) {
        Ok(client) => {
            let status = client.status().map_err(|error| error.to_string())?;
            println!(
                "running (pid {}, uptime {}s, clients {}, state {})",
                status.pid,
                status.uptime_ms / 1000,
                status.connections,
                status.state_path
            );
            Ok(())
        }
        Err(SdkError::DaemonUnavailable) => Err("not running".into()),
        Err(error) => Err(error.to_string()),
    }
}

fn stop(args: &[String]) -> Result<(), String> {
    let options = parse_options(args)?;
    let endpoint = endpoint(&options);
    let client =
        Client::connect_with_timeout(Some(&endpoint), Duration::from_secs(2)).map_err(|error| {
            match error {
                SdkError::DaemonUnavailable => "no daemon is running".to_string(),
                other => other.to_string(),
            }
        })?;
    client.shutdown().map_err(|error| error.to_string())?;
    drop(client);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match RemoteBackend::connect_with_timeout(Some(&endpoint), Duration::from_millis(200)) {
            Ok(_) => std::thread::sleep(Duration::from_millis(100)),
            Err(SdkError::DaemonUnavailable) => {
                println!("stopped");
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("daemon did not stop within 5 seconds".into())
}

fn selftest(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("selftest takes no options".into());
    }
    let checks = com0com_daemon::selftest().map_err(|error| error.to_string())?;
    println!("com0comd selftest: ok ({checks} checks)");
    Ok(())
}
