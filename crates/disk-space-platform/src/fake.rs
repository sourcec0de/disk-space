//! Deterministic fake platform harness for offline tests and headless UI verification.

use std::fs;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
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

/// Test double for [`crate::traits::PlatformHost`] that avoids OS trash and GUI calls.
#[derive(Debug)]
pub struct FakePlatform {
    volume: VolumeInfo,
    protected_paths: Vec<PathBuf>,
    trashed: Mutex<Vec<PathBuf>>,
    revealed: Mutex<Vec<PathBuf>>,
    opened: Mutex<Vec<PathBuf>>,
    previewed: Mutex<Vec<PathBuf>>,
    device_overrides: Mutex<Vec<(PathBuf, DeviceId)>>,
}

impl FakePlatform {
    /// Builds a `FakePlatform` anchored at `root` with a simulated 500 GB volume.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let device_id = Self::probe_device(&root);
        let volume = VolumeInfo::new(
            "Test Volume".to_owned(),
            root,
            500_000_000_000,
            250_000_000_000,
            250_000_000_000,
            "apfs".to_owned(),
            false,
            true,
            device_id,
        );
        Self {
            volume,
            protected_paths: Vec::new(),
            trashed: Mutex::new(Vec::new()),
            revealed: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
            previewed: Mutex::new(Vec::new()),
            device_overrides: Mutex::new(Vec::new()),
        }
    }

    /// Registers an additional protected path for safety-jail tests.
    #[must_use]
    pub fn with_protected_path(mut self, path: PathBuf) -> Self {
        self.protected_paths.push(path);
        self
    }

    /// Overrides the reported [`DeviceId`] for a path prefix to simulate mount boundaries.
    pub fn set_device_override(&self, path: PathBuf, device_id: DeviceId) {
        if let Ok(mut guard) = self.device_overrides.lock() {
            guard.push((path, device_id));
        }
    }

    /// Returns paths passed to `move_to_trash`.
    #[must_use]
    pub fn trashed_paths(&self) -> Vec<PathBuf> {
        self.trashed
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Returns paths passed to `reveal_in_file_manager`.
    #[must_use]
    pub fn revealed_paths(&self) -> Vec<PathBuf> {
        self.revealed
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Returns paths passed to `open_item`.
    #[must_use]
    pub fn opened_paths(&self) -> Vec<PathBuf> {
        self.opened
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Returns paths passed to `preview_item`.
    #[must_use]
    pub fn previewed_paths(&self) -> Vec<PathBuf> {
        self.previewed
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn probe_device(path: &Path) -> DeviceId {
        #[cfg(unix)]
        {
            crate::unix::lstat_unix(path).map_or(DeviceId::new(1), |meta| meta.device_id)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            DeviceId::new(1)
        }
    }

    fn override_device(&self, path: &Path) -> Option<DeviceId> {
        let guard = self.device_overrides.lock().ok()?;
        guard
            .iter()
            .find(|(prefix, _)| path == prefix || path.starts_with(prefix))
            .map(|(_, dev)| *dev)
    }
}

impl VolumeProvider for FakePlatform {
    fn list_volumes(&self) -> Result<Vec<VolumeInfo>, PlatformError> {
        Ok(vec![self.volume.clone()])
    }

    fn volume_for_path(&self, _path: &Path) -> Result<VolumeInfo, PlatformError> {
        Ok(self.volume.clone())
    }
}

impl FileMetadataInspector for FakePlatform {
    fn lstat(&self, path: &Path) -> Result<EntryMetadata, PlatformError> {
        #[cfg(unix)]
        let mut meta = crate::unix::lstat_unix(path)?;
        #[cfg(not(unix))]
        let mut meta = {
            let fs_meta = fs::symlink_metadata(path).map_err(|source| PlatformError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let ft = fs_meta.file_type();
            let kind = if ft.is_dir() {
                EntryKind::Directory
            } else if ft.is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            EntryMetadata::new(
                kind,
                fs_meta.len(),
                fs_meta.len(),
                DeviceId::new(1),
                0,
                1,
                fs_meta.modified().ok(),
                fs_meta.accessed().ok(),
                false,
            )
        };
        if let Some(dev) = self.override_device(path) {
            meta.device_id = dev;
        }
        Ok(meta)
    }

    fn open_regular_nofollow_rw(&self, path: &Path) -> Result<File, PlatformError> {
        #[cfg(unix)]
        {
            crate::unix::open_regular_nofollow_rw_unix(path)
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|source| PlatformError::Io {
                    path: path.to_path_buf(),
                    source,
                })
        }
    }
}

impl PermissionAdvisor for FakePlatform {
    fn access_hint(&self, unreadable_count: usize) -> Option<AccessHint> {
        if unreadable_count == 0 {
            return None;
        }
        Some(AccessHint::new(
            "grant Full Disk Access".to_owned(),
            "Grant Full Disk Access".to_owned(),
            None,
        ))
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Ok(())
    }
}

impl CategoryClassifier for FakePlatform {
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

impl ProtectedPathPolicy for FakePlatform {
    fn protection_reason(&self, canonical_path: &Path) -> Option<ProtectionReason> {
        if canonical_path == Path::new("/") {
            return Some(ProtectionReason::SystemRoot);
        }
        if self
            .protected_paths
            .iter()
            .any(|protected| canonical_path == protected || canonical_path.starts_with(protected))
        {
            return Some(ProtectionReason::OperatingSystemCore);
        }
        None
    }

    fn should_skip_scan_entry(&self, _path: &Path, _scan_root: &Path) -> bool {
        false
    }
}

impl FileActionBackend for FakePlatform {
    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), PlatformError> {
        if let Ok(mut guard) = self.revealed.lock() {
            guard.push(path.to_path_buf());
        }
        Ok(())
    }

    fn open_item(&self, path: &Path) -> Result<(), PlatformError> {
        if let Ok(mut guard) = self.opened.lock() {
            guard.push(path.to_path_buf());
        }
        Ok(())
    }

    fn preview_item(&self, path: &Path) -> Result<(), PlatformError> {
        if let Ok(mut guard) = self.previewed.lock() {
            guard.push(path.to_path_buf());
        }
        Ok(())
    }

    fn move_to_trash(&self, path: &Path) -> Result<(), PlatformError> {
        let meta = self.lstat(path)?;
        if meta.kind == EntryKind::Directory {
            fs::remove_dir_all(path).map_err(|source| PlatformError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        } else {
            fs::remove_file(path).map_err(|source| PlatformError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        if let Ok(mut guard) = self.trashed.lock() {
            guard.push(path.to_path_buf());
        }
        Ok(())
    }
}
