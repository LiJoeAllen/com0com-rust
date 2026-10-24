//! Driver service diagnostics: service state, image metadata and event-log
//! entries for `com0combus` / `com0comport`.

use std::ffi::c_void;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr;

use com0com_core::{DriverInfo, DriverKind, DriverStatus, LogEntry, LogLevel, SdkError};
use sha2::{Digest, Sha256};
use windows_sys::Win32::errhandlingapi::GetLastError;
use windows_sys::Win32::winerror::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, ERROR_SERVICE_DOES_NOT_EXIST,
};
use windows_sys::Win32::winevt::{
    EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender,
    EvtRenderEventXml, EVT_HANDLE,
};
use windows_sys::Win32::winsvc::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfigW, QueryServiceStatusEx,
    QUERY_SERVICE_CONFIGW, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_CONFIG,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS_PROCESS,
    SERVICE_STOPPED, SERVICE_STOP_PENDING,
};
use windows_sys::Win32::winver::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};

pub(crate) fn service_name(kind: DriverKind) -> &'static str {
    match kind {
        DriverKind::Bus => "com0combus",
        DriverKind::Port => "com0comport",
    }
}

pub(crate) fn driver_info(kind: DriverKind) -> Result<DriverInfo, SdkError> {
    let name = service_name(kind);
    let mut info = DriverInfo {
        kind,
        status: DriverStatus::NotInstalled,
        service_name: name.to_string(),
        image_path: None,
        version: None,
        sha256: None,
    };

    unsafe {
        let manager = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT as u32);
        if manager.is_null() {
            return Err(last_error("OpenSCManagerW"));
        }
        let manager = ScHandle(manager);
        let name_wide = wide(name);
        let service = OpenServiceW(
            manager.0,
            name_wide.as_ptr(),
            (SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) as u32,
        );
        if service.is_null() {
            let code = GetLastError();
            if code == ERROR_SERVICE_DOES_NOT_EXIST as u32 {
                return Ok(info);
            }
            return Err(win32_error("OpenServiceW", code));
        }
        let service = ScHandle(service);

        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        if QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            &mut status as *mut SERVICE_STATUS_PROCESS as *mut u8,
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        ) == 0
        {
            return Err(last_error("QueryServiceStatusEx"));
        }
        info.status = map_state(status.dwCurrentState);

        if let Some(raw) = query_image_path(service.0) {
            let path = normalize_image_path(&raw);
            info.image_path = Some(path.display().to_string());
            info.version = file_version(&path);
            info.sha256 = file_sha256(&path);
        }
    }

    Ok(info)
}

pub(crate) fn driver_logs(limit: usize) -> Result<Vec<LogEntry>, SdkError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let channel = wide("System");
    let query = wide("*[System[Provider[@Name='com0combus' or @Name='com0comport']]]");
    unsafe {
        let set = EvtQuery(
            ptr::null_mut(),
            channel.as_ptr(),
            query.as_ptr(),
            (EvtQueryChannelPath | EvtQueryReverseDirection) as u32,
        );
        if set.is_null() {
            return Err(last_error("EvtQuery"));
        }
        let set = EvtHandle(set);
        let mut entries = Vec::new();
        let mut events: [EVT_HANDLE; 16] = [ptr::null_mut(); 16];
        while entries.len() < limit {
            let mut returned = 0u32;
            if EvtNext(
                set.0,
                events.len() as u32,
                events.as_mut_ptr(),
                0,
                0,
                &mut returned,
            ) == 0
            {
                let code = GetLastError();
                if code == ERROR_NO_MORE_ITEMS as u32 {
                    break;
                }
                return Err(win32_error("EvtNext", code));
            }
            for &event in &events[..returned as usize] {
                let entry = render_event(event);
                EvtClose(event);
                if let Some(entry) = entry {
                    if entries.len() < limit {
                        entries.push(entry);
                    }
                }
            }
        }
        Ok(entries)
    }
}

pub(crate) fn map_state(state: u32) -> DriverStatus {
    match state as i32 {
        SERVICE_RUNNING => DriverStatus::Running,
        SERVICE_STOPPED => DriverStatus::Stopped,
        SERVICE_START_PENDING => DriverStatus::StartPending,
        SERVICE_STOP_PENDING => DriverStatus::StopPending,
        _ => DriverStatus::Unknown,
    }
}

pub(crate) fn normalize_image_path(raw: &str) -> PathBuf {
    let value = raw.trim().trim_matches('"');
    if let Some(rest) = value.strip_prefix(r"\??\") {
        return PathBuf::from(rest);
    }
    if let Some(rest) = value.strip_prefix(r"\SystemRoot\") {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        return root.join(rest);
    }
    PathBuf::from(value)
}

pub(crate) fn parse_event_xml(xml: &str) -> LogEntry {
    let timestamp = xml
        .split_once("SystemTime=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(value, _)| value.to_string())
        .unwrap_or_default();
    let level = element_text(xml, "Level")
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(map_level)
        .unwrap_or(LogLevel::Unknown);
    let event_id = element_text(xml, "EventID").unwrap_or("?");
    let mut parts = Vec::new();
    let mut rest = xml;
    while let Some((_, after)) = rest.split_once("<Data") {
        let Some((_, content)) = after.split_once('>') else {
            break;
        };
        let Some((value, tail)) = content.split_once("</Data>") else {
            break;
        };
        let value = unescape(value);
        if !value.is_empty() {
            parts.push(value);
        }
        rest = tail;
    }
    let message = if parts.is_empty() {
        format!("Event ID {event_id}")
    } else {
        format!("Event ID {event_id}: {}", parts.join(" "))
    };
    LogEntry {
        timestamp,
        level,
        message,
    }
}

fn map_level(level: u32) -> LogLevel {
    match level {
        1 | 2 => LogLevel::Error,
        3 => LogLevel::Warning,
        4 => LogLevel::Information,
        5 => LogLevel::Verbose,
        _ => LogLevel::Unknown,
    }
}

fn element_text<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}");
    let start = xml.find(&open)?;
    let after = &xml[start..];
    let content = &after[after.find('>')? + 1..];
    let close = format!("</{name}>");
    let end = content.find(&close)?;
    Some(&content[..end])
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn query_image_path(service: windows_sys::Win32::winsvc::SC_HANDLE) -> Option<String> {
    unsafe {
        let mut needed = 0u32;
        QueryServiceConfigW(service, ptr::null_mut(), 0, &mut needed);
        if GetLastError() != ERROR_INSUFFICIENT_BUFFER as u32 || needed == 0 {
            return None;
        }
        // `QUERY_SERVICE_CONFIGW` contains pointers, so the buffer must be
        // pointer-aligned; a `u64` vector provides that on every Windows ABI.
        let words = (needed as usize).div_ceil(size_of::<u64>());
        let mut buffer = vec![0u64; words];
        if QueryServiceConfigW(
            service,
            buffer.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW,
            needed,
            &mut needed,
        ) == 0
        {
            return None;
        }
        let config = &*(buffer.as_ptr() as *const QUERY_SERVICE_CONFIGW);
        let path = wide_to_string(config.lpBinaryPathName);
        (!path.is_empty()).then_some(path)
    }
}

fn file_version(path: &Path) -> Option<String> {
    let wide_path = wide(&path.to_string_lossy());
    unsafe {
        let mut handle = 0u32;
        let size = GetFileVersionInfoSizeW(wide_path.as_ptr(), &mut handle);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(
            wide_path.as_ptr(),
            handle,
            size,
            data.as_mut_ptr() as *mut c_void,
        ) == 0
        {
            return None;
        }
        let sub_block = wide("\\");
        let mut value = ptr::null_mut();
        let mut length = 0u32;
        if VerQueryValueW(
            data.as_ptr() as *const c_void,
            sub_block.as_ptr(),
            &mut value,
            &mut length,
        ) == 0
            || value.is_null()
            || (length as usize) < size_of::<VsFixedFileInfo>()
        {
            return None;
        }
        let fixed = &*(value as *const VsFixedFileInfo);
        if fixed.signature != 0xfeef_04bd {
            return None;
        }
        Some(format!(
            "{}.{}.{}.{}",
            fixed.file_version_ms >> 16,
            fixed.file_version_ms & 0xffff,
            fixed.file_version_ls >> 16,
            fixed.file_version_ls & 0xffff
        ))
    }
}

fn file_sha256(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn render_event(event: EVT_HANDLE) -> Option<LogEntry> {
    let mut used = 0u32;
    let mut properties = 0u32;
    let mut buffer = vec![0u16; 16 * 1024];
    unsafe {
        let ok = EvtRender(
            ptr::null_mut(),
            event,
            EvtRenderEventXml as u32,
            (buffer.len() * 2) as u32,
            buffer.as_mut_ptr() as *mut c_void,
            &mut used,
            &mut properties,
        );
        if ok == 0 {
            let code = GetLastError();
            if code != ERROR_INSUFFICIENT_BUFFER as u32 {
                return None;
            }
            buffer = vec![0u16; (used as usize).div_ceil(2).max(1)];
            if EvtRender(
                ptr::null_mut(),
                event,
                EvtRenderEventXml as u32,
                (buffer.len() * 2) as u32,
                buffer.as_mut_ptr() as *mut c_void,
                &mut used,
                &mut properties,
            ) == 0
            {
                return None;
            }
        }
    }
    let length = (used as usize / 2).min(buffer.len());
    let xml = String::from_utf16_lossy(&buffer[..length]);
    Some(parse_event_xml(xml.trim_end_matches('\0')))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_to_string(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe {
        let mut length = 0usize;
        while *pointer.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    }
}

fn last_error(op: &'static str) -> SdkError {
    win32_error(op, unsafe { GetLastError() })
}

fn win32_error(op: &'static str, code: u32) -> SdkError {
    if code == 5 || code == 1314 {
        SdkError::PermissionDenied
    } else {
        SdkError::Win32 {
            op: op.to_string(),
            code,
            hint: None,
        }
    }
}

struct ScHandle(windows_sys::Win32::winsvc::SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

struct EvtHandle(EVT_HANDLE);

impl Drop for EvtHandle {
    fn drop(&mut self) {
        unsafe {
            EvtClose(self.0);
        }
    }
}

/// `VS_FIXEDFILEINFO` from `ver.h`; windows-sys does not expose it.
#[repr(C)]
struct VsFixedFileInfo {
    signature: u32,
    struct_version: u32,
    file_version_ms: u32,
    file_version_ls: u32,
    product_version_ms: u32,
    product_version_ls: u32,
    file_flags_mask: u32,
    file_flags: u32,
    file_os: u32,
    file_type: u32,
    file_subtype: u32,
    file_date_ms: u32,
    file_date_ls: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_service_states() {
        assert_eq!(map_state(SERVICE_RUNNING as u32), DriverStatus::Running);
        assert_eq!(map_state(SERVICE_STOPPED as u32), DriverStatus::Stopped);
        assert_eq!(
            map_state(SERVICE_START_PENDING as u32),
            DriverStatus::StartPending
        );
        assert_eq!(
            map_state(SERVICE_STOP_PENDING as u32),
            DriverStatus::StopPending
        );
        assert_eq!(map_state(7), DriverStatus::Unknown);
    }

    #[test]
    fn normalizes_service_paths() {
        assert_eq!(
            normalize_image_path(r"\??\C:\drivers\com0combus.sys"),
            PathBuf::from(r"C:\drivers\com0combus.sys")
        );
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        assert_eq!(
            normalize_image_path(r"\SystemRoot\System32\drivers\com0combus.sys"),
            system_root.join(r"System32\drivers\com0combus.sys")
        );
        assert_eq!(
            normalize_image_path(r#""C:\drivers\com0comport.sys""#),
            PathBuf::from(r"C:\drivers\com0comport.sys")
        );
    }

    #[test]
    fn parses_event_xml() {
        let xml = r#"<Event><System><Provider Name="com0combus"/><EventID Qualifiers="0">17</EventID><Level>2</Level><TimeCreated SystemTime="2026-10-23T12:34:56.0000000Z"/></System><EventData><Data Name="Name">\Device\com0com</Data><Data Name="Status">0xC0000001 &amp; more</Data></EventData></Event>"#;
        let entry = parse_event_xml(xml);
        assert_eq!(entry.timestamp, "2026-10-23T12:34:56.0000000Z");
        assert_eq!(entry.level, LogLevel::Error);
        assert_eq!(
            entry.message,
            r"Event ID 17: \Device\com0com 0xC0000001 & more"
        );
    }

    #[test]
    fn parses_event_without_data() {
        let xml = r#"<Event><System><EventID>5</EventID><Level>4</Level></System></Event>"#;
        let entry = parse_event_xml(xml);
        assert_eq!(entry.level, LogLevel::Information);
        assert_eq!(entry.message, "Event ID 5");
        assert!(entry.timestamp.is_empty());
    }
}
