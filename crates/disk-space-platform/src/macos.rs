//! macOS platform backend using POSIX `statfs`, APFS awareness, and `AppKit` tools.

#![cfg(target_os = "macos")]

use std::env;
use std::ffi::CStr;
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
use crate::types::DeviceId;
use crate::types::EntryKind;
use crate::types::EntryMetadata;
use crate::types::JunkAssessment;
use crate::types::ProtectionReason;
use crate::types::StorageCategory;
use crate::types::VolumeInfo;
use crate::unix;

/// Native macOS implementation of the platform harness.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct MacOsPlatform;

impl MacOsPlatform {
    /// Creates a new macOS platform host.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[expect(
    unsafe_code,
    reason = "POSIX statfs FFI call has no safe stdlib equivalent"
)]
fn query_statfs(path: &Path) -> Result<(VolumeInfo, DeviceId), PlatformError> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| PlatformError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"),
    })?;

    let mut stat_buf = MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c_path` is valid NUL-terminated UTF-8/bytes and `stat_buf` points to
    // properly aligned writable memory of `libc::statfs`.
    let rc = unsafe { libc::statfs(c_path.as_ptr(), stat_buf.as_mut_ptr()) };
    if rc != 0 {
        return Err(unix::map_io_error(path, io::Error::last_os_error()));
    }

    // SAFETY: `libc::statfs` succeeded (`rc == 0`) and fully initialized `stat_buf`.
    let stat = unsafe { stat_buf.assume_init() };

    // SAFETY: `f_fstypename` is a NUL-terminated C char array populated by `statfs`.
    let fs_type = unsafe { CStr::from_ptr(stat.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .into_owned();

    // SAFETY: `f_mntonname` is a NUL-terminated C char array populated by `statfs`.
    let mount_str = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) }
        .to_string_lossy()
        .into_owned();

    let mount_point = PathBuf::from(&mount_str);
    let block_size = u64::from(stat.f_bsize);
    let total_bytes = stat.f_blocks.saturating_mul(block_size);
    let free_bytes = stat.f_bfree.saturating_mul(block_size);
    let available_bytes = stat.f_bavail.saturating_mul(block_size);
    let used_bytes = total_bytes.saturating_sub(free_bytes);

    let meta = unix::lstat_unix(path)?;
    let is_removable = (stat.f_flags & u32::try_from(libc::MNT_LOCAL).unwrap_or(0)) == 0
        || mount_str.starts_with("/Volumes/");
    let is_cow_or_ssd = fs_type.eq_ignore_ascii_case("apfs");
    let name = friendly_volume_name(&mount_point);

    Ok((
        VolumeInfo::new(
            name,
            mount_point,
            total_bytes,
            used_bytes,
            available_bytes,
            fs_type,
            is_removable,
            is_cow_or_ssd,
            meta.device_id,
        ),
        meta.device_id,
    ))
}

fn friendly_volume_name(mount_point: &Path) -> String {
    if mount_point == Path::new("/") || mount_point == Path::new("/System/Volumes/Data") {
        if let Ok(entries) = fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if let Ok(target) = fs::read_link(&candidate)
                    && (target == Path::new("/") || target == Path::new("/System/Volumes/Data"))
                    && let Some(label) = candidate.file_name()
                {
                    return label.to_string_lossy().into_owned();
                }
            }
        }
        return "Macintosh HD".to_owned();
    }

    mount_point
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Volume".to_owned())
}

impl VolumeProvider for MacOsPlatform {
    fn list_volumes(&self) -> Result<Vec<VolumeInfo>, PlatformError> {
        let mut volumes = Vec::new();
        if let Ok((root_vol, _)) = query_statfs(Path::new("/")) {
            volumes.push(root_vol);
        }
        if let Ok(entries) = fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if fs::read_link(&candidate).is_ok() {
                    continue;
                }
                if let Ok((vol, _)) = query_statfs(&candidate)
                    && !volumes
                        .iter()
                        .any(|existing| existing.mount_point == vol.mount_point)
                {
                    volumes.push(vol);
                }
            }
        }
        if volumes.is_empty() {
            return Err(PlatformError::Unsupported {
                operation: "no accessible macOS volumes found",
            });
        }
        Ok(volumes)
    }

    fn volume_for_path(&self, path: &Path) -> Result<VolumeInfo, PlatformError> {
        query_statfs(path).map(|(vol, _)| vol)
    }
}

impl FileMetadataInspector for MacOsPlatform {
    fn lstat(&self, path: &Path) -> Result<EntryMetadata, PlatformError> {
        unix::lstat_unix(path)
    }

    fn open_regular_nofollow_rw(&self, path: &Path) -> Result<File, PlatformError> {
        unix::open_regular_nofollow_rw_unix(path)
    }
}

impl PermissionAdvisor for MacOsPlatform {
    fn access_hint(&self, unreadable_count: usize) -> Option<AccessHint> {
        if unreadable_count == 0 {
            return None;
        }
        Some(AccessHint::new(
            "grant Full Disk Access".to_owned(),
            "Grant Full Disk Access".to_owned(),
            Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
                    .to_owned(),
            ),
        ))
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        let status = Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .status()
            .map_err(|err| PlatformError::CommandFailed {
                command: "open",
                message: err.to_string(),
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(PlatformError::CommandFailed {
                command: "open",
                message: format!("exited with status {status}"),
            })
        }
    }
}

impl CategoryClassifier for MacOsPlatform {
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

impl ProtectedPathPolicy for MacOsPlatform {
    fn protection_reason(&self, canonical_path: &Path) -> Option<ProtectionReason> {
        const PROTECTED_PREFIXES: [&str; 9] = [
            "/System",
            "/usr",
            "/bin",
            "/sbin",
            "/Library/Apple",
            "/private/var/db",
            "/private/etc",
            "/dev",
            "/Volumes",
        ];

        if canonical_path == Path::new("/") {
            return Some(ProtectionReason::SystemRoot);
        }

        if let Some(home) = env::var_os("HOME").map(PathBuf::from)
            && let Ok(canonical_home) = fs::canonicalize(home)
            && canonical_path == canonical_home
        {
            return Some(ProtectionReason::UserHomeRoot);
        }

        for prefix in PROTECTED_PREFIXES {
            let protected = Path::new(prefix);
            if canonical_path == protected || canonical_path.starts_with(protected) {
                return Some(if prefix == "/dev" {
                    ProtectionReason::VirtualFilesystem
                } else {
                    ProtectionReason::OperatingSystemCore
                });
            }
        }

        None
    }

    fn should_skip_scan_entry(&self, path: &Path, scan_root: &Path) -> bool {
        if path == Path::new("/dev") || path == Path::new("/.vol") {
            return true;
        }
        if scan_root == Path::new("/")
            && matches!(
                path.to_str(),
                Some(
                    "/System/Volumes/Data"
                        | "/System/Volumes/VM"
                        | "/System/Volumes/Preboot"
                        | "/System/Volumes/Update"
                        | "/Volumes"
                )
            )
        {
            return true;
        }
        false
    }
}

impl FileActionBackend for MacOsPlatform {
    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), PlatformError> {
        run_open_command(&["-R"], path)
    }

    fn open_item(&self, path: &Path) -> Result<(), PlatformError> {
        run_open_command(&[], path)
    }

    fn move_to_trash(&self, path: &Path) -> Result<(), PlatformError> {
        trash::delete(path).map_err(|err| PlatformError::TrashFailed {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    }
}

fn run_open_command(flags: &[&str], path: &Path) -> Result<(), PlatformError> {
    let mut cmd = Command::new("open");
    cmd.args(flags).arg(path);
    let status = cmd.status().map_err(|err| PlatformError::CommandFailed {
        command: "open",
        message: err.to_string(),
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::CommandFailed {
            command: "open",
            message: format!("exited with status {status}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_volume_queries_successfully() {
        let host = MacOsPlatform::new();
        let volume = host.volume_for_path(Path::new("/")).expect("statfs root");
        assert!(volume.total_bytes > 0);
        assert_eq!(volume.mount_point, Path::new("/"));
    }

    #[test]
    fn protected_paths_block_root_and_system() {
        let host = MacOsPlatform::new();
        assert_eq!(
            host.protection_reason(Path::new("/")),
            Some(ProtectionReason::SystemRoot)
        );
        assert_eq!(
            host.protection_reason(Path::new("/System/Library")),
            Some(ProtectionReason::OperatingSystemCore)
        );
        assert_eq!(
            host.protection_reason(Path::new("/tmp/disk-space-safe-test")),
            None
        );
    }

    #[test]
    fn nofollow_rejects_symlinks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.bin");
        fs::write(&target, b"hello").expect("write target");
        let link = temp.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let host = MacOsPlatform::new();
        let link_meta = host.lstat(&link).expect("lstat symlink");
        assert_eq!(link_meta.kind, EntryKind::Symlink);
        assert!(host.open_regular_nofollow_rw(&link).is_err());
        assert!(host.open_regular_nofollow_rw(&target).is_ok());
    }
}
