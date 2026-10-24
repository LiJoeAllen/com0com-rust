//! Port link directories and safe symlink handling.
//!
//! A PTY slave lives in `/dev/pts/N` (Linux) or `/dev/ttysNNN` (macOS), which
//! changes on every restart. Each managed port gets a stable symlink:
//! `<dir>/<label>` on Linux, `<dir>/cu.com0com-<label>` on macOS (plus a
//! `tty.com0com-<label>` alias). A root daemon writes into `/dev`; an
//! unprivileged process uses a private directory.

use std::path::{Path, PathBuf};

use com0com_core::SdkError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkStyle {
    Linux,
    MacOs,
}

#[derive(Clone, Debug)]
pub(crate) struct LinkConfig {
    pub(crate) dir: PathBuf,
    pub(crate) style: LinkStyle,
}

fn platform_style() -> LinkStyle {
    if cfg!(target_os = "macos") {
        LinkStyle::MacOs
    } else {
        LinkStyle::Linux
    }
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn user_device_dir(style: LinkStyle) -> Result<PathBuf, SdkError> {
    if style == LinkStyle::Linux {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(runtime).join("com0com"));
        }
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or(SdkError::PermissionDenied)?;
    Ok(match style {
        LinkStyle::Linux => home.join(".local/state/com0com/dev"),
        LinkStyle::MacOs => home.join("Library/Application Support/com0com/ports"),
    })
}

impl LinkConfig {
    /// Resolve the directory from `COM0COM_DEV_DIR`, the caller's privileges and
    /// the platform default. The directory is created with 0755 for a root
    /// daemon and 0700 otherwise.
    pub(crate) fn resolve() -> Result<Self, SdkError> {
        let style = platform_style();
        let (dir, mode) =
            if let Some(dir) = std::env::var_os("COM0COM_DEV_DIR").filter(|v| !v.is_empty()) {
                (PathBuf::from(dir), 0o700)
            } else if is_root() {
                let dir = match style {
                    LinkStyle::Linux => PathBuf::from("/dev/com0com"),
                    LinkStyle::MacOs => PathBuf::from("/dev"),
                };
                (dir, 0o755)
            } else {
                (user_device_dir(style)?, 0o700)
            };
        Self::at(dir, style, mode)
    }

    /// Test and daemon plumbing: use an explicit directory.
    pub(crate) fn at(
        dir: impl Into<PathBuf>,
        style: LinkStyle,
        mode: u32,
    ) -> Result<Self, SdkError> {
        use std::os::unix::fs::PermissionsExt;
        let dir = dir.into();
        // macOS shares `/dev` with the OS; chmod only a directory we created.
        let created = std::fs::symlink_metadata(&dir).is_err();
        std::fs::create_dir_all(&dir)
            .map_err(|error| SdkError::Io(format!("create {}: {error}", dir.display())))?;
        if created {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))
                .map_err(|error| SdkError::Io(format!("chmod {}: {error}", dir.display())))?;
        }
        Ok(Self { dir, style })
    }

    pub(crate) fn link_path(&self, label: &str) -> PathBuf {
        match self.style {
            LinkStyle::Linux => self.dir.join(label),
            LinkStyle::MacOs => self.dir.join(format!("cu.com0com-{label}")),
        }
    }

    /// macOS applications sometimes insist on the blocking `tty.*` node.
    pub(crate) fn alias_path(&self, label: &str) -> Option<PathBuf> {
        match self.style {
            LinkStyle::MacOs => Some(self.dir.join(format!("tty.com0com-{label}"))),
            LinkStyle::Linux => None,
        }
    }
}

/// True when `target` is a PTY slave node. Used before replacing a stale link
/// left behind by a crashed daemon: regular files are never replaced.
pub(crate) fn is_pty_target(target: &Path) -> bool {
    let text = target.to_string_lossy();
    if text.starts_with("/dev/pts/") {
        return true;
    }
    match target.file_name().and_then(|name| name.to_str()) {
        Some(name) => {
            text.starts_with("/dev/") && (name.starts_with("ttys") || name.starts_with("pty"))
        }
        None => false,
    }
}

/// Create `link -> target`, replacing only a stale PTY link.
pub(crate) fn create_link(link: &Path, target: &Path) -> Result<(), SdkError> {
    match std::fs::symlink_metadata(link) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Err(SdkError::AlreadyExists);
            }
            let existing = std::fs::read_link(link)
                .map_err(|error| SdkError::Io(format!("readlink {}: {error}", link.display())))?;
            if !is_pty_target(&existing) {
                return Err(SdkError::AlreadyExists);
            }
            std::fs::remove_file(link)
                .map_err(|error| SdkError::Io(format!("remove {}: {error}", link.display())))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SdkError::Io(format!("lstat {}: {error}", link.display())));
        }
    }
    std::os::unix::fs::symlink(target, link)
        .map_err(|error| SdkError::Io(format!("symlink {}: {error}", link.display())))
}

/// Remove `link` only when it still points at `target` and is a symlink.
pub(crate) fn remove_link(link: &Path, target: &Path) {
    if let Ok(metadata) = std::fs::symlink_metadata(link) {
        if metadata.file_type().is_symlink()
            && std::fs::read_link(link).ok().as_deref() == Some(target)
        {
            let _ = std::fs::remove_file(link);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let dir = std::env::temp_dir().join(format!(
            "com0com-links-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn creates_replaces_and_removes_only_pty_links() {
        let dir = scratch("links");
        let cfg = LinkConfig::at(&dir, LinkStyle::Linux, 0o700).unwrap();
        let link = cfg.link_path("A0");
        assert_eq!(link, dir.join("A0"));

        create_link(&link, Path::new("/dev/pts/1")).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("/dev/pts/1"));

        // A stale PTY link is replaced by the next daemon run.
        create_link(&link, Path::new("/dev/pts/2")).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("/dev/pts/2"));

        // A regular file in the way is never overwritten.
        let occupied = dir.join("B0");
        std::fs::write(&occupied, b"keep me").unwrap();
        assert_eq!(
            create_link(&occupied, Path::new("/dev/pts/3")),
            Err(SdkError::AlreadyExists)
        );
        assert_eq!(std::fs::read(&occupied).unwrap(), b"keep me");

        // Removing with the wrong target leaves the link alone.
        remove_link(&link, Path::new("/dev/pts/9"));
        assert!(std::fs::symlink_metadata(&link).is_ok());
        remove_link(&link, Path::new("/dev/pts/2"));
        assert!(std::fs::symlink_metadata(&link).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn macos_links_use_cu_and_tty_prefixes() {
        let dir = scratch("mac");
        let cfg = LinkConfig::at(&dir, LinkStyle::MacOs, 0o700).unwrap();
        assert_eq!(cfg.link_path("A1"), dir.join("cu.com0com-A1"));
        assert_eq!(cfg.alias_path("A1"), Some(dir.join("tty.com0com-A1")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_target_detection() {
        assert!(is_pty_target(Path::new("/dev/pts/4")));
        assert!(is_pty_target(Path::new("/dev/ttys004")));
        assert!(!is_pty_target(Path::new("/etc/passwd")));
        assert!(!is_pty_target(Path::new("/dev/null")));
    }
}
