//! Windows platform backend using standard filesystem metadata, Explorer, and Recycle Bin.

#![cfg(target_os = "windows")]

use std::env;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
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

/// Native Windows implementation of the platform harness.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct WindowsPlatform;

impl WindowsPlatform {
    /// Creates a new Windows platform host.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl VolumeProvider for WindowsPlatform {
    fn list_volumes(&self) -> Result<Vec<VolumeInfo>, PlatformError> {
        Ok(vec![self.volume_for_path(Path::new("C:\\"))?])
    }

    fn volume_for_path(&self, path: &Path) -> Result<VolumeInfo, PlatformError> {
        let root = path
            .ancestors()
            .last()
            .map_or_else(|| PathBuf::from("C:\\"), Path::to_path_buf);
        let name = root.to_string_lossy().into_owned();
        Ok(VolumeInfo::new(
            name,
            root,
            0,
            0,
            0,
            "ntfs".to_owned(),
            false,
            true,
            DeviceId::new(1),
        ))
    }
}

impl FileMetadataInspector for WindowsPlatform {
    fn lstat(&self, path: &Path) -> Result<EntryMetadata, PlatformError> {
        let meta = fs::symlink_metadata(path).map_err(|source| map_io_error(path, source))?;
        let file_type = meta.file_type();
        let kind = if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        let is_hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'));
        Ok(EntryMetadata::new(
            kind,
            meta.len(),
            meta.len(),
            DeviceId::new(1),
            0,
            1,
            meta.modified().ok(),
            meta.accessed().ok(),
            is_hidden,
        ))
    }

    fn open_regular_nofollow_rw(&self, path: &Path) -> Result<File, PlatformError> {
        let meta = self.lstat(path)?;
        if meta.kind != EntryKind::File {
            return Err(PlatformError::Io {
                path: path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::InvalidInput, "target is not a regular file"),
            });
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| map_io_error(path, source))
    }
}

impl PermissionAdvisor for WindowsPlatform {
    fn access_hint(&self, unreadable_count: usize) -> Option<AccessHint> {
        if unreadable_count == 0 {
            return None;
        }
        Some(AccessHint::new(
            "run as Administrator for protected folders".to_owned(),
            "Access Info".to_owned(),
            None,
        ))
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported {
            operation: "UAC settings deep link",
        })
    }
}

impl CategoryClassifier for WindowsPlatform {
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

impl ProtectedPathPolicy for WindowsPlatform {
    fn protection_reason(&self, canonical_path: &Path) -> Option<ProtectionReason> {
        if canonical_path.parent().is_none() {
            return Some(ProtectionReason::SystemRoot);
        }
        if let Some(home) = env::var_os("USERPROFILE").map(PathBuf::from)
            && let Ok(canonical_home) = fs::canonicalize(home)
            && canonical_path == canonical_home
        {
            return Some(ProtectionReason::UserHomeRoot);
        }
        let lower = canonical_path.to_string_lossy().to_ascii_lowercase();
        if lower.contains("\\windows") || lower.contains("\\program files") {
            return Some(ProtectionReason::OperatingSystemCore);
        }
        None
    }

    fn should_skip_scan_entry(&self, path: &Path, _scan_root: &Path) -> bool {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        lower.ends_with("\\system volume information")
    }
}

impl FileActionBackend for WindowsPlatform {
    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), PlatformError> {
        let arg = format!("/select,{}", path.display());
        let status = Command::new("explorer").arg(arg).status().map_err(|err| {
            PlatformError::CommandFailed {
                command: "explorer",
                message: err.to_string(),
            }
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(PlatformError::CommandFailed {
                command: "explorer",
                message: format!("exited with status {status}"),
            })
        }
    }

    fn open_item(&self, path: &Path) -> Result<(), PlatformError> {
        let status = Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status()
            .map_err(|err| PlatformError::CommandFailed {
                command: "cmd",
                message: err.to_string(),
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(PlatformError::CommandFailed {
                command: "cmd",
                message: format!("exited with status {status}"),
            })
        }
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

fn map_io_error(path: &Path, source: io::Error) -> PlatformError {
    if source.kind() == io::ErrorKind::PermissionDenied {
        PlatformError::PermissionDenied {
            path: path.to_path_buf(),
        }
    } else {
        PlatformError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}
