//! Linux platform backend using POSIX `statvfs`, XDG conventions, and `xdg-open`.

#![cfg(target_os = "linux")]

use std::env;
use std::ffi::CString;
use std::fs;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;

use crate::classify;
use crate::error::PlatformError;
use crate::traits::CategoryClassifier;
use crate::traits::FileActionBackend;
use crate::traits::FileMetadataInspector;
use crate::traits::PermissionAdvisor;
use crate::traits::ProtectedPathPolicy;
use crate::traits::VolumeProvider;
use crate::types::AccessHint;
use crate::types::EntryKind;
use crate::types::EntryMetadata;
use crate::types::JunkAssessment;
use crate::types::ProtectionReason;
use crate::types::StorageCategory;
use crate::types::VolumeInfo;
use crate::unix;

/// Native Linux implementation of the platform harness.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct LinuxPlatform;

impl LinuxPlatform {
    /// Creates a new Linux platform host.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[expect(
    unsafe_code,
    reason = "POSIX statvfs FFI call has no safe stdlib equivalent"
)]
fn query_statvfs(path: &Path) -> Result<VolumeInfo, PlatformError> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| PlatformError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"),
    })?;

    let mut stat_buf = MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c_path` is a NUL-terminated path and `stat_buf` points to valid `statvfs` storage.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat_buf.as_mut_ptr()) };
    if rc != 0 {
        return Err(unix::map_io_error(path, io::Error::last_os_error()));
    }
    // SAFETY: `statvfs` succeeded (`rc == 0`) and initialized all fields.
    let stat = unsafe { stat_buf.assume_init() };

    let frsize = if stat.f_frsize > 0 {
        stat.f_frsize
    } else {
        stat.f_bsize
    };
    let total_bytes = stat.f_blocks.saturating_mul(frsize);
    let free_bytes = stat.f_bfree.saturating_mul(frsize);
    let available_bytes = stat.f_bavail.saturating_mul(frsize);
    let used_bytes = total_bytes.saturating_sub(free_bytes);

    let meta = unix::lstat_unix(path)?;
    let name = path
        .file_name()
        .map(|label| label.to_string_lossy().into_owned())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "Root".to_owned());

    Ok(VolumeInfo::new(
        name,
        path.to_path_buf(),
        total_bytes,
        used_bytes,
        available_bytes,
        "posix".to_owned(),
        false,
        true,
        meta.device_id,
    ))
}

impl VolumeProvider for LinuxPlatform {
    fn list_volumes(&self) -> Result<Vec<VolumeInfo>, PlatformError> {
        Ok(vec![query_statvfs(Path::new("/"))?])
    }

    fn volume_for_path(&self, path: &Path) -> Result<VolumeInfo, PlatformError> {
        query_statvfs(path)
    }
}

impl FileMetadataInspector for LinuxPlatform {
    fn lstat(&self, path: &Path) -> Result<EntryMetadata, PlatformError> {
        unix::lstat_unix(path)
    }

    fn open_regular_nofollow_rw(&self, path: &Path) -> Result<File, PlatformError> {
        unix::open_regular_nofollow_rw_unix(path)
    }
}

impl PermissionAdvisor for LinuxPlatform {
    fn access_hint(&self, unreadable_count: usize) -> Option<AccessHint> {
        if unreadable_count == 0 {
            return None;
        }
        Some(AccessHint::new(
            "some folders require elevated permissions".to_owned(),
            "View Permissions".to_owned(),
            None,
        ))
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported {
            operation: "permission settings pane on Linux",
        })
    }
}

impl CategoryClassifier for LinuxPlatform {
    fn classify(
        &self,
        path: &Path,
        kind: EntryKind,
        metadata: &EntryMetadata,
        now: SystemTime,
    ) -> (StorageCategory, JunkAssessment) {
        classify::classify_entry(path, kind, metadata, now)
    }
}

impl ProtectedPathPolicy for LinuxPlatform {
    fn protection_reason(&self, canonical_path: &Path) -> Option<ProtectionReason> {
        if canonical_path == Path::new("/") {
            return Some(ProtectionReason::SystemRoot);
        }
        if let Some(home) = env::var_os("HOME").map(PathBuf::from)
            && let Ok(canonical_home) = fs::canonicalize(home)
            && canonical_path == canonical_home
        {
            return Some(ProtectionReason::UserHomeRoot);
        }

        for virtual_prefix in ["/proc", "/sys", "/dev", "/run"] {
            if canonical_path.starts_with(virtual_prefix) {
                return Some(ProtectionReason::VirtualFilesystem);
            }
        }
        for core_prefix in ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/boot", "/etc"] {
            if canonical_path.starts_with(core_prefix) {
                return Some(ProtectionReason::OperatingSystemCore);
            }
        }
        None
    }

    fn should_skip_scan_entry(&self, path: &Path, _scan_root: &Path) -> bool {
        matches!(path.to_str(), Some("/proc" | "/sys" | "/dev" | "/run"))
    }
}

impl FileActionBackend for LinuxPlatform {
    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), PlatformError> {
        let target = path.parent().unwrap_or(path);
        run_xdg_open(target)
    }

    fn open_item(&self, path: &Path) -> Result<(), PlatformError> {
        run_xdg_open(path)
    }

    fn preview_item(&self, path: &Path) -> Result<(), PlatformError> {
        self.open_item(path)
    }

    fn move_to_trash(&self, path: &Path) -> Result<(), PlatformError> {
        trash::delete(path).map_err(|err| PlatformError::TrashFailed {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    }
}

fn run_xdg_open(path: &Path) -> Result<(), PlatformError> {
    let status = Command::new("xdg-open").arg(path).status().map_err(|err| {
        PlatformError::CommandFailed {
            command: "xdg-open",
            message: err.to_string(),
        }
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::CommandFailed {
            command: "xdg-open",
            message: format!("exited with status {status}"),
        })
    }
}
