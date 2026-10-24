//! `setupc` — thin command-line front end over the com0com SDK.
//!
//! Command names, output text and exit codes are kept byte-compatible with the
//! original Win32 IOCTL implementation; all device access now goes through
//! `com0com-sdk`.

mod args;
mod output;

use com0com_core::{PairId, MAX_PAIR_ID};
use com0com_sdk::Sdk;

fn open_sdk() -> Result<Sdk, String> {
    #[cfg(unix)]
    {
        // PTY pairs live inside the process that created them, so the daemon
        // is the default owner. `COM0COM_BACKEND` forces one mode when a test
        // or a service needs to bypass auto-detection.
        match std::env::var("COM0COM_BACKEND").as_deref() {
            Ok("native") => return Sdk::open_native().map_err(|e| e.to_string()),
            Ok("daemon") => return Sdk::connect_daemon(None).map_err(|e| e.to_string()),
            Ok("") | Err(_) => {}
            Ok(other) => {
                return Err(format!(
                    "unknown COM0COM_BACKEND={other} (expected native or daemon)"
                ))
            }
        }
        Sdk::open().map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        if std::env::var_os("COM0COM_DAEMON").is_some_and(|value| !value.is_empty()) {
            return Sdk::connect_daemon(None).map_err(|e| e.to_string());
        }
        Sdk::open().map_err(|e| e.to_string())
    }
}

fn u32_to_id(raw: u32) -> Result<PairId, String> {
    u8::try_from(raw)
        .ok()
        .filter(|id| *id <= MAX_PAIR_ID)
        .ok_or_else(|| format!("invalid pair id: {raw} (0..={MAX_PAIR_ID})"))
}

fn parse_id(text: &str) -> Result<PairId, String> {
    let raw = text.parse::<u32>().map_err(|_| "invalid pair id")?;
    u32_to_id(raw)
}

fn cmd_list() -> Result<(), String> {
    let sdk = open_sdk()?;
    let pairs = sdk.list_pairs().map_err(|e| e.to_string())?;
    println!("{}", output::LIST_HEADER);
    if pairs.is_empty() {
        println!("(no pairs)");
        return Ok(());
    }
    for pair in &pairs {
        println!("{}", output::format_pair_row(pair));
    }
    Ok(())
}

fn cmd_add(args: &[String]) -> Result<(), String> {
    let pa = args::parse_pair_args(args)?;
    let id = match pa.id {
        Some(raw) => Some(u32_to_id(raw)?),
        None => None,
    };
    let sdk = open_sdk()?;
    let pair = sdk.create_pair(id, &pa.params).map_err(|e| e.to_string())?;
    println!("Created pair {}.", pair.id);
    Ok(())
}

fn cmd_remove(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("usage: setupc remove <id>".into());
    }
    let id = parse_id(&args[0])?;
    let sdk = open_sdk()?;
    sdk.delete_pair(id).map_err(|e| e.to_string())?;
    println!("Removed pair {id}.");
    Ok(())
}

fn cmd_params(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("usage: setupc params <id>".into());
    }
    let id = parse_id(&args[0])?;
    let sdk = open_sdk()?;
    let params = sdk.get_params(id).map_err(|e| e.to_string())?;
    println!("{}", output::params_block(id, &params));
    Ok(())
}

fn cmd_set(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: setupc set <id> [flags...]".into());
    }
    let id = parse_id(&args[0])?;
    let pa = args::parse_pair_args(&args[1..])?;
    let sdk = open_sdk()?;
    sdk.set_params(id, &pa.params).map_err(|e| e.to_string())?;
    println!("Updated pair {id}.");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        output::print_help();
        return;
    }
    let result = match args[1].as_str() {
        "list" => cmd_list(),
        "add" => cmd_add(&args[2..]),
        "remove" => cmd_remove(&args[2..]),
        "params" => cmd_params(&args[2..]),
        "set" => cmd_set(&args[2..]),
        "-h" | "--help" | "help" => {
            output::print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            output::print_help();
            std::process::exit(1);
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
