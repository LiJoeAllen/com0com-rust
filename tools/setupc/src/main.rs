//! `setupc` — user-mode configuration tool for the Rust com0com rewrite.
//!
//! It locates the bus driver's control device interface (`GUID_COM0COM_BUS`),
//! opens it, and issues the private `IOCTL_COM0COM_*` control codes to
//! create / delete / list virtual null-modem pairs and to read or write their
//! emulation parameters.
//!
//! This is the user-mode counterpart of the C com0com `setupc`/`setupg` tool.
//! Only the kernel driver is Rust; the wire protocol is the same idea.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING};
use windows_sys::Win32::System::IO::{DeviceIoControl, OVERLAPPED};

use com0com_common::constants::{DEFAULT_PIN_WIRING, GUID_COM0COM_BUS_BYTES};
use com0com_common::ioctls;
use com0com_common::protocol::{
    Com0comPairParams, CreatePairRequest, DeletePairRequest, EnumPairsResponse, GetParamsRequest,
    SetParamsRequest,
};

// ---------------------------------------------------------------------------
// Small helpers: wide strings, struct (de)serialization, GUID conversion.
// ---------------------------------------------------------------------------

fn to_wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// Diagnostic trace, enabled by setting the `COM0COM_TRACE` environment
/// variable. Writes to stderr so normal command output stays parseable.
fn trace(msg: &str) {
    if std::env::var_os("COM0COM_TRACE").is_some() {
        eprintln!("[setupc] {msg}");
    }
}

fn wide_buf_to_string(buf: &[u16]) -> String {
    match buf.iter().position(|&c| c == 0) {
        Some(end) => String::from_utf16_lossy(&buf[..end]),
        None => String::from_utf16_lossy(buf),
    }
}

/// Build a `windows_sys::core::GUID` from the 16 little-endian GUID bytes used
/// throughout `com0com-common`.
fn guid_from_bytes(b: &[u8; 16]) -> GUID {
    let data1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let data2 = u16::from_le_bytes([b[4], b[5]]);
    let data3 = u16::from_le_bytes([b[6], b[7]]);
    let mut data4 = [0u8; 8];
    data4.copy_from_slice(&b[8..16]);
    GUID {
        data1,
        data2,
        data3,
        data4,
    }
}

unsafe fn from_bytes<T: Copy>(buf: &[u8]) -> T {
    let mut v = std::mem::MaybeUninit::<T>::uninit();
    std::ptr::copy_nonoverlapping(
        buf.as_ptr(),
        v.as_mut_ptr() as *mut u8,
        size_of::<T>().min(buf.len()),
    );
    v.assume_init()
}

// ---------------------------------------------------------------------------
// Locating & opening the bus control device.
// ---------------------------------------------------------------------------

/// Return the device path of the first present `GUID_COM0COM_BUS` interface.
fn bus_device_path() -> Result<String, String> {
    trace("bus_device_path: enter");
    let guid = guid_from_bytes(&GUID_COM0COM_BUS_BYTES);
    let hdev: HDEVINFO = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(), // hwndparent: HWND (*mut c_void) — no parent window.
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    // HDEVINFO 在 0.61 中仍为 isize，而 INVALID_HANDLE_VALUE 已是 HANDLE
    // （= *mut c_void，值为 -1），两者类型不同，需先转型再比较。
    if hdev == INVALID_HANDLE_VALUE as isize {
        return Err(format!("SetupDiGetClassDevsW failed (Win32 {})", unsafe {
            GetLastError()
        }));
    }

    trace("bus_device_path: class devs ok");
    let mut didata: SP_DEVICE_INTERFACE_DATA = unsafe { zeroed() };
    didata.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

    let found = unsafe { SetupDiEnumDeviceInterfaces(hdev, null(), &guid, 0, &mut didata) };
    if found == 0 {
        unsafe { SetupDiDestroyDeviceInfoList(hdev) };
        return Err(format!(
            "No com0com bus interface found (Win32 {}). Is com0combus.sys loaded?",
            unsafe { GetLastError() }
        ));
    }

    trace("bus_device_path: interface enumerated");
    // 第一次调用：查询设备路径所需缓冲区大小，预期以 ERROR_INSUFFICIENT_BUFFER 失败。
    let mut required: u32 = 0;
    let probe_ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(hdev, &didata, null_mut(), 0, &mut required, null_mut())
    };
    let probe_err = unsafe { GetLastError() };
    if probe_ok != 0 || probe_err != ERROR_INSUFFICIENT_BUFFER || required == 0 {
        unsafe { SetupDiDestroyDeviceInfoList(hdev) };
        return Err(format!(
            "SetupDiGetDeviceInterfaceDetailW size probe failed (Win32 {}, required {})",
            probe_err, required
        ));
    }

    // 用 usize 元素分配，保证缓冲区满足 SP_DEVICE_INTERFACE_DETAIL_DATA_W 的对齐要求。
    let words = (required as usize + size_of::<usize>() - 1) / size_of::<usize>();
    let mut buf: Vec<usize> = vec![0; words];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // cbSize 取文档规定的固定部分大小：x64 为 8（含对齐填充），x86 为 6。
    let detail_cb_size: u32 = if size_of::<usize>() == 8 { 8 } else { 6 };
    unsafe {
        (*detail).cbSize = detail_cb_size;
    }
    trace(&format!(
        "bus_device_path: size probe ok, required={required}"
    ));
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(hdev, &didata, detail, required, null_mut(), null_mut())
    };
    trace(&format!("bus_device_path: detail ok={ok}"));
    let path = if ok != 0 {
        unsafe {
            let path_ptr = &(*detail).DevicePath as *const u16;
            let mut len = 0usize;
            while *path_ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(path_ptr, len);
            String::from_utf16_lossy(slice)
        }
    } else {
        unsafe { SetupDiDestroyDeviceInfoList(hdev) };
        return Err(format!(
            "SetupDiGetDeviceInterfaceDetailW failed (Win32 {})",
            unsafe { GetLastError() }
        ));
    };

    unsafe { SetupDiDestroyDeviceInfoList(hdev) };
    Ok(path)
}

/// Open the bus control device for read+write (covers both READ and WRITE
/// access IOCTLs).
fn open_bus() -> Result<HANDLE, String> {
    let path = bus_device_path()?;
    trace(&format!("open_bus: path={path}"));
    let wide = to_wide(&path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(), // htemplatefile: HANDLE (*mut c_void) — not a file template.
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "CreateFileW('{}') failed (Win32 {}). Try running as Administrator.",
            path,
            unsafe { GetLastError() }
        ));
    }
    trace("open_bus: handle open");
    Ok(handle)
}

/// Zero-copy view of a `#[repr(C)]` `Copy` struct as a byte slice, for use as a
/// `DeviceIoControl` input buffer. SAFE: the slice aliases `v` immutably and the
/// kernel only reads it for the duration of the call, so there is no aliasing
/// hazard and no need to copy the request into a separate buffer.
fn struct_bytes<T: Copy>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>()) }
}

/// Deserialize a `#[repr(C)]` `Copy` struct from a byte slice. The single
/// `memcpy` here is mandatory: `DeviceIoControl` wrote raw bytes into our buffer
/// and the target struct may require stricter alignment than a `u8` buffer.
fn struct_of<T: Copy>(buf: &[u8]) -> T {
    unsafe { from_bytes(buf) }
}

/// Close a bus handle — SAFE wrapper over `CloseHandle`.
fn close_bus(h: HANDLE) {
    unsafe { CloseHandle(h) };
}

/// Thin, SAFE wrapper around `DeviceIoControl`. All `unsafe` (the FFI call and
/// the raw pointer casts) is confined here; callers never write `unsafe`.
fn ioctl(handle: HANDLE, code: u32, in_buf: &[u8], out_buf: &mut [u8]) -> Result<u32, u32> {
    trace(&format!(
        "ioctl: code=0x{code:08x} in={} out={}",
        in_buf.len(),
        out_buf.len()
    ));
    let mut bytes_returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            code,
            if in_buf.is_empty() {
                std::ptr::null::<c_void>()
            } else {
                in_buf.as_ptr() as *const c_void
            },
            in_buf.len() as u32,
            if out_buf.is_empty() {
                std::ptr::null_mut::<c_void>()
            } else {
                out_buf.as_mut_ptr() as *mut c_void
            },
            out_buf.len() as u32,
            &mut bytes_returned,
            null_mut::<OVERLAPPED>(),
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        trace(&format!("ioctl: failed win32={err}"));
        Err(err)
    } else {
        trace(&format!("ioctl: ok bytes={bytes_returned}"));
        Ok(bytes_returned)
    }
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

fn cmd_list() -> Result<(), String> {
    let handle = open_bus()?;
    let mut out = vec![0u8; size_of::<EnumPairsResponse>()];
    let res = ioctl(handle, ioctls::IOCTL_COM0COM_ENUM_PAIRS, &[], &mut out);
    close_bus(handle);
    let bytes = res.map_err(|e| format!("ENUM_PAIRS failed (Win32 {e})"))?;

    let resp: EnumPairsResponse = struct_of(&out[..bytes as usize]);
    println!("Pair  PortA      PortB      Flags");
    println!("----  ---------  ---------  ----------------------------");
    if resp.count == 0 {
        println!("(no pairs)");
        return Ok(());
    }
    for entry in resp.entries.iter().take(resp.count as usize) {
        let a = wide_buf_to_string(&entry.params.port_name[0]);
        let b = wide_buf_to_string(&entry.params.port_name[1]);
        let flags = describe_flags(&entry.params);
        println!(
            "{:<5} {:<11} {:<11} {}",
            entry.pair_id,
            if a.is_empty() { "(auto)" } else { a.as_str() },
            if b.is_empty() { "(auto)" } else { b.as_str() },
            flags
        );
    }
    Ok(())
}

fn describe_flags(p: &Com0comPairParams) -> String {
    let mut f: Vec<String> = Vec::new();
    if p.emu_br != 0 {
        f.push("baud-emu".to_string());
    }
    if p.emu_overrun != 0 {
        f.push("overrun".to_string());
    }
    if p.emu_noise != 0 {
        f.push(format!("noise({}%)", p.noise_probability / 100));
    }
    if p.exclusive_mode != 0 {
        f.push("exclusive".to_string());
    }
    if p.plugin_mode != 0 {
        f.push("plugin".to_string());
    }
    if f.is_empty() {
        "default".to_string()
    } else {
        f.join(", ")
    }
}

/// Parse the shared `--id / --port-a / --port-b / --baud-emu / --overrun /
/// --noise[=prob] / --exclusive / --plugin` arguments. `--id` is only relevant
/// for `add`; for `set` it is ignored.
struct PairArgs {
    id: Option<u32>,
    params: Com0comPairParams,
}

fn parse_pair_args(args: &[String]) -> Result<PairArgs, String> {
    let mut pa = PairArgs {
        id: None,
        params: Com0comPairParams {
            pin_wiring: DEFAULT_PIN_WIRING,
            ..Default::default()
        },
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--id" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or("--id requires a value")?
                    .parse::<u32>()
                    .map_err(|_| "invalid --id value")?;
                pa.id = Some(v);
            }
            "--port-a" | "--port-b" => {
                let is_a = a == "--port-a";
                i += 1;
                let name = args.get(i).ok_or("--port-x requires a value")?.clone();
                let wide = to_wide(&name);
                if wide.len() > 32 {
                    return Err("--port-x name too long (max 31 chars)".into());
                }
                let mut arr = [0u16; 32];
                arr[..wide.len()].copy_from_slice(&wide);
                if is_a {
                    pa.params.port_name[0] = arr;
                } else {
                    pa.params.port_name[1] = arr;
                }
            }
            "--baud-emu" => pa.params.emu_br = 1,
            "--overrun" => pa.params.emu_overrun = 1,
            "--noise" => pa.params.emu_noise = 1,
            "--exclusive" => pa.params.exclusive_mode = 1,
            "--plugin" => pa.params.plugin_mode = 1,
            other => {
                // `--noise=3000` style.
                if let Some(prob) = other.strip_prefix("--noise=") {
                    pa.params.emu_noise = 1;
                    pa.params.noise_probability = prob
                        .parse::<u32>()
                        .map_err(|_| "invalid --noise=prob value")?;
                } else {
                    return Err(format!("unknown argument: {other}"));
                }
            }
        }
        i += 1;
    }
    Ok(pa)
}

fn find_free_id(handle: HANDLE) -> Result<u32, String> {
    let mut out = vec![0u8; size_of::<EnumPairsResponse>()];
    let bytes = ioctl(handle, ioctls::IOCTL_COM0COM_ENUM_PAIRS, &[], &mut out)
        .map_err(|e| format!("ENUM_PAIRS failed (Win32 {e})"))?;
    let resp: EnumPairsResponse = struct_of(&out[..bytes as usize]);
    let used: std::collections::HashSet<u32> = resp
        .entries
        .iter()
        .take(resp.count as usize)
        .map(|e| e.pair_id)
        .collect();
    for id in 0..64u32 {
        if !used.contains(&id) {
            return Ok(id);
        }
    }
    Err("no free pair id (limit 64 reached)".into())
}

fn cmd_add(args: &[String]) -> Result<(), String> {
    let pa = parse_pair_args(args)?;
    let handle = open_bus()?;
    let id = match pa.id {
        Some(v) => v,
        None => find_free_id(handle)?,
    };
    let req = CreatePairRequest {
        pair_id: id,
        params: pa.params,
    };
    let req_bytes = struct_bytes(&req);
    let res = ioctl(
        handle,
        ioctls::IOCTL_COM0COM_CREATE_PAIR,
        req_bytes,
        &mut [],
    );
    close_bus(handle);
    res.map_err(|e| format!("CREATE_PAIR failed (Win32 {e})"))?;
    println!("Created pair {id}.");
    Ok(())
}

fn cmd_remove(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("usage: setupc remove <id>".into());
    }
    let id = args[0].parse::<u32>().map_err(|_| "invalid pair id")?;
    let handle = open_bus()?;
    let req = DeletePairRequest { pair_id: id };
    let req_bytes = struct_bytes(&req);
    let res = ioctl(
        handle,
        ioctls::IOCTL_COM0COM_DELETE_PAIR,
        req_bytes,
        &mut [],
    );
    close_bus(handle);
    res.map_err(|e| format!("DELETE_PAIR failed (Win32 {e})"))?;
    println!("Removed pair {id}.");
    Ok(())
}

fn cmd_params(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("usage: setupc params <id>".into());
    }
    let id = args[0].parse::<u32>().map_err(|_| "invalid pair id")?;
    let handle = open_bus()?;
    let req = GetParamsRequest {
        pair_id: id,
        port_index: 0,
    };
    let req_bytes = struct_bytes(&req);
    let mut out = vec![0u8; size_of::<Com0comPairParams>()];
    let res = ioctl(
        handle,
        ioctls::IOCTL_COM0COM_GET_PARAMS,
        req_bytes,
        &mut out,
    );
    close_bus(handle);
    res.map_err(|e| format!("GET_PARAMS failed (Win32 {e})"))?;
    let p: Com0comPairParams = struct_of(&out);
    let a = wide_buf_to_string(&p.port_name[0]);
    let b = wide_buf_to_string(&p.port_name[1]);
    println!("Pair {id}");
    println!(
        "  PortA:      {}",
        if a.is_empty() { "(auto)" } else { a.as_str() }
    );
    println!(
        "  PortB:      {}",
        if b.is_empty() { "(auto)" } else { b.as_str() }
    );
    println!("  Flags:      {}", describe_flags(&p));
    println!(
        "  PinWiring:  A={:02x?} B={:02x?}",
        p.pin_wiring[0], p.pin_wiring[1]
    );
    Ok(())
}

fn cmd_set(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: setupc set <id> [flags...]".into());
    }
    // First positional is the id; the rest are params.
    let id = args[0].parse::<u32>().map_err(|_| "invalid pair id")?;
    let pa = parse_pair_args(&args[1..])?;
    let handle = open_bus()?;
    let req = SetParamsRequest {
        pair_id: id,
        port_index: 0,
        params: pa.params,
    };
    let req_bytes = struct_bytes(&req);
    let res = ioctl(handle, ioctls::IOCTL_COM0COM_SET_PARAMS, req_bytes, &mut []);
    close_bus(handle);
    res.map_err(|e| format!("SET_PARAMS failed (Win32 {e})"))?;
    println!("Updated pair {id}.");
    Ok(())
}

fn print_help() {
    println!(
        "setupc - configure the Rust com0com virtual null-modem driver

USAGE:
  setupc list
  setupc add   [--id N] [--port-a NAME] [--port-b NAME]
               [--baud-emu] [--overrun] [--noise[=PROB]] [--exclusive] [--plugin]
  setupc remove <id>
  setupc params <id>
  setupc set   <id> [same flags as add, minus --id/--port-*]

PORT NAME:
  Omit --port-a/--port-b to let the COM Name Arbiter assign COMx.
  Pairs created with an explicit id persist that logical pairing id; the bus
  auto-assigns the lowest free id when --id is omitted.

EXAMPLES:
  setupc add --port-a COM15 --port-b COM16
  setupc add --baud-emu --noise=500        (5% noise, auto ids/names)
  setupc list
  setupc params 0
  setupc set 0 --overrun
  setupc remove 0

NOTE:
  Requires com0combus.sys to be loaded and run with Administrator rights.
"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        return;
    }
    let result = match args[1].as_str() {
        "list" => cmd_list(),
        "add" => cmd_add(&args[2..]),
        "remove" => cmd_remove(&args[2..]),
        "params" => cmd_params(&args[2..]),
        "set" => cmd_set(&args[2..]),
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            std::process::exit(1);
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_full_flag_set() {
        let a = args(&[
            "--id",
            "5",
            "--port-a",
            "COM15",
            "--port-b",
            "COM16",
            "--baud-emu",
            "--overrun",
            "--noise=3000",
            "--exclusive",
            "--plugin",
        ]);
        let pa = parse_pair_args(&a).expect("should parse");
        assert_eq!(pa.id, Some(5));
        assert_eq!(wide_buf_to_string(&pa.params.port_name[0]), "COM15");
        assert_eq!(wide_buf_to_string(&pa.params.port_name[1]), "COM16");
        assert_eq!(pa.params.emu_br, 1);
        assert_eq!(pa.params.emu_overrun, 1);
        assert_eq!(pa.params.emu_noise, 1);
        assert_eq!(pa.params.noise_probability, 3000);
        assert_eq!(pa.params.exclusive_mode, 1);
        assert_eq!(pa.params.plugin_mode, 1);
        // The parser seeds the default null-modem pin wiring.
        assert_eq!(pa.params.pin_wiring, DEFAULT_PIN_WIRING);
    }

    #[test]
    fn parse_bare_noise_sets_flag_without_probability() {
        let pa = parse_pair_args(&args(&["--noise"])).expect("should parse");
        assert_eq!(pa.params.emu_noise, 1);
        assert_eq!(pa.params.noise_probability, 0);
    }

    #[test]
    fn parse_defaults_when_no_flags() {
        let pa = parse_pair_args(&[]).expect("should parse");
        assert_eq!(pa.id, None);
        assert_eq!(pa.params.emu_br, 0);
        assert_eq!(pa.params.noise_probability, 0);
        assert_eq!(pa.params.pin_wiring, DEFAULT_PIN_WIRING);
        // An unset port name decodes to an empty string.
        assert_eq!(wide_buf_to_string(&pa.params.port_name[0]), "");
    }

    #[test]
    fn parse_accepts_31_char_port_name() {
        let name = "C".repeat(31);
        let pa = parse_pair_args(&args(&["--port-a", &name])).expect("should parse");
        assert_eq!(pa.params.port_name[0][31], 0);
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        assert!(parse_pair_args(&args(&["--bogus"])).is_err());
        assert!(parse_pair_args(&args(&["rofl"])).is_err());
    }

    #[test]
    fn parse_rejects_missing_or_bad_values() {
        assert!(parse_pair_args(&args(&["--id"])).is_err());
        assert!(parse_pair_args(&args(&["--id", "notanumber"])).is_err());
        assert!(parse_pair_args(&args(&["--port-a"])).is_err());
        assert!(parse_pair_args(&args(&["--noise=abc"])).is_err());
    }

    #[test]
    fn parse_rejects_too_long_port_name() {
        // 32 chars + NUL = 33 wide units, exceeding the fixed 32-slot buffer.
        let name = "C".repeat(32);
        assert!(parse_pair_args(&args(&["--port-a", &name])).is_err());
    }

    #[test]
    fn describe_flags_variants() {
        let p = Com0comPairParams::default();
        assert_eq!(describe_flags(&p), "default");

        let p = Com0comPairParams {
            emu_br: 1,
            ..Default::default()
        };
        assert_eq!(describe_flags(&p), "baud-emu");

        // noise_probability is 0.01% units: 3000 -> 30%.
        let p = Com0comPairParams {
            emu_noise: 1,
            noise_probability: 3000,
            ..Default::default()
        };
        assert_eq!(describe_flags(&p), "noise(30%)");

        // Order follows describe_flags: baud-emu, overrun, noise, exclusive, plugin.
        let p = Com0comPairParams {
            emu_br: 1,
            emu_overrun: 1,
            exclusive_mode: 1,
            plugin_mode: 1,
            ..Default::default()
        };
        assert_eq!(describe_flags(&p), "baud-emu, overrun, exclusive, plugin");
    }

    #[test]
    fn wide_string_helpers_roundtrip() {
        assert_eq!(wide_buf_to_string(&to_wide("COM12")), "COM12");
        let w: [u16; 6] = [b'A' as u16, b'B' as u16, 0, b'X' as u16, b'Y' as u16, 0];
        assert_eq!(wide_buf_to_string(&w), "AB");
    }

    #[test]
    fn guid_from_bytes_matches_bus() {
        let g = guid_from_bytes(&GUID_COM0COM_BUS_BYTES);
        // GUID_COM0COM_BUS = 900C0CC0-2211-4433-5566-778899AABBCC
        assert_eq!(g.data1, 0x900C_0CC0);
        assert_eq!(g.data2, 0x2211);
        assert_eq!(g.data3, 0x4433);
        assert_eq!(g.data4, [0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC]);
    }
}
