//! Cross-platform trait interfaces implemented by each OS host backend.

use std::fmt::Debug;
use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use crate::error::PlatformError;
use crate::types::AccessHint;
use crate::types::EntryKind;
use crate::types::EntryMetadata;
use crate::types::JunkAssessment;
use crate::types::ProtectionReason;
use crate::types::StorageCategory;
use crate::types::VolumeInfo;

/// Queries mounted storage volumes and resolves volume metadata for a path.
pub trait VolumeProvider: Send + Sync {
    /// Lists visible local volumes.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the host cannot enumerate or stat mounted volumes.
    fn list_volumes(&self) -> Result<Vec<VolumeInfo>, PlatformError>;

    /// Returns volume capacity and device information for the mount containing `path`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if `path` cannot be resolved or queried on the host.
    fn volume_for_path(&self, path: &Path) -> Result<VolumeInfo, PlatformError>;
}

/// Inspects entry metadata without following symlinks and opens files safely for erasure.
pub trait FileMetadataInspector: Send + Sync {
    /// Inspects `path` without dereferencing symbolic links (`lstat`).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::PermissionDenied`] or [`PlatformError::Io`] on stat failure.
    fn lstat(&self, path: &Path) -> Result<EntryMetadata, PlatformError>;

    /// Opens an existing regular file for read/write without following symbolic links.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if `path` is a symlink, not a regular file, or cannot be opened.
    fn open_regular_nofollow_rw(&self, path: &Path) -> Result<File, PlatformError>;
}

/// Reports OS permission remediation hints (e.g. macOS Full Disk Access).
pub trait PermissionAdvisor: Send + Sync {
    /// Produces a user-facing hint when `unreadable_count > 0` directories could not be read.
    fn access_hint(&self, unreadable_count: usize) -> Option<AccessHint>;

    /// Opens the OS permission settings pane when available.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if launching settings fails or is unsupported.
    fn open_permission_settings(&self) -> Result<(), PlatformError>;
}

/// Classifies paths and metadata into storage categories and junk/unused heuristics.
pub trait CategoryClassifier: Send + Sync {
    /// Maps a filesystem entry to a [`StorageCategory`] and [`JunkAssessment`].
    fn classify(
        &self,
        path: &Path,
        kind: EntryKind,
        metadata: &EntryMetadata,
        now: SystemTime,
    ) -> (StorageCategory, JunkAssessment);
}

/// Enforces OS-specific protected paths and virtual filesystem scan exclusions.
pub trait ProtectedPathPolicy: Send + Sync {
    /// Returns `Some(ProtectionReason)` if `canonical_path` must never be deleted.
    fn protection_reason(&self, canonical_path: &Path) -> Option<ProtectionReason>;

    /// Returns `true` if `path` should be skipped during traversal under `scan_root`
    /// (for example virtual filesystems or macOS firmlink duplicates under `/`).
    fn should_skip_scan_entry(&self, path: &Path, scan_root: &Path) -> bool;
}

/// Executes desktop file manager and OS trash actions.
pub trait FileActionBackend: Send + Sync {
    /// Highlights `path` in the OS file manager (Finder, Explorer, file manager).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if revealing fails.
    fn reveal_in_file_manager(&self, path: &Path) -> Result<(), PlatformError>;

    /// Opens `path` using the OS default handler.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if opening fails.
    fn open_item(&self, path: &Path) -> Result<(), PlatformError>;

    /// Previews `path` in the OS quick-preview viewer (macOS Quick Look `qlmanage -p`,
    /// or the default opener on platforms without a native previewer).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if launching the preview fails.
    fn preview_item(&self, path: &Path) -> Result<(), PlatformError>;

    /// Moves `path` to the OS Trash / Recycle Bin.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if moving to trash fails.
    fn move_to_trash(&self, path: &Path) -> Result<(), PlatformError>;
}

/// Combined platform harness required by the scanner, safety jail, erasure engine, and UI.
pub trait PlatformHost:
    VolumeProvider
    + FileMetadataInspector
    + PermissionAdvisor
    + CategoryClassifier
    + ProtectedPathPolicy
    + FileActionBackend
    + Debug
    + Send
    + Sync
{
}

impl<T> PlatformHost for T where
    T: VolumeProvider
        + FileMetadataInspector
        + PermissionAdvisor
        + CategoryClassifier
        + ProtectedPathPolicy
        + FileActionBackend
        + Debug
        + Send
        + Sync
{
}
