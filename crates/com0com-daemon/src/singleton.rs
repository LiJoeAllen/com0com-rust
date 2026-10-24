//! Single-instance guard for the daemon.
//!
//! Windows uses a local named mutex, Unix an exclusive `flock` on a lock file
//! next to the control socket. Both are released by the OS when the process
//! exits, so a crash never leaves a stale lock.

use com0com_core::SdkError;

pub struct Singleton {
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
    #[cfg(unix)]
    _lock: std::fs::File,
}

// The Windows guard is a kernel handle owned exclusively by this process.
unsafe impl Send for Singleton {}

impl Singleton {
    pub fn acquire(endpoint: &str) -> Result<Self, SdkError> {
        imp::acquire(endpoint)
    }
}

#[cfg(windows)]
mod imp {
    use std::ptr::null;

    use com0com_core::SdkError;
    use windows_sys::Win32::errhandlingapi::GetLastError;
    use windows_sys::Win32::handleapi::CloseHandle;
    use windows_sys::Win32::synchapi::CreateMutexW;
    use windows_sys::Win32::winerror::ERROR_ALREADY_EXISTS;

    use super::Singleton;

    fn fnv1a(value: &str) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in value.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn acquire(endpoint: &str) -> Result<Singleton, SdkError> {
        // One mutex per endpoint keeps parallel test daemons on distinct pipes
        // independent while still rejecting a second daemon on the same pipe.
        let name = format!(r"Local\com0comd-{:016x}", fnv1a(endpoint));
        let name = wide(&name);
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() {
            let code = unsafe { GetLastError() };
            return Err(SdkError::Win32 {
                op: "CreateMutexW".into(),
                code,
                hint: None,
            });
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS as u32 {
            unsafe {
                CloseHandle(handle);
            }
            return Err(SdkError::Busy);
        }
        Ok(Singleton { handle })
    }

    impl Drop for Singleton {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    use std::path::{Path, PathBuf};

    use com0com_core::SdkError;

    use super::Singleton;

    fn lock_path(endpoint: &str) -> PathBuf {
        let path = Path::new(endpoint);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "control.sock".into());
        path.with_file_name(format!(".{file_name}.lock"))
    }

    pub fn acquire(endpoint: &str) -> Result<Singleton, SdkError> {
        let path = lock_path(endpoint);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| SdkError::Io(error.to_string()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| SdkError::Io(format!("open lock {}: {error}", path.display())))?;
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(SdkError::Busy);
            }
            return Err(SdkError::Io(format!("lock {}: {error}", path.display())));
        }
        Ok(Singleton { _lock: file })
    }
}
