//! Cross-platform value types shared between the OS harness, core, and UI.

use std::path::PathBuf;
use std::time::SystemTime;

/// Opaque identifier of a mounted filesystem or storage device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct DeviceId(u64);

impl DeviceId {
    /// Creates a `DeviceId` from a raw platform device number.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the raw platform device number.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Kind of filesystem entry returned by `lstat` without following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link (never followed by scans or deletions).
    Symlink,
    /// Special entry (FIFO, socket, block/character device, etc.).
    Other,
}

/// Filesystem metadata for a single path obtained via `lstat`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EntryMetadata {
    /// Kind of directory entry.
    pub kind: EntryKind,
    /// Logical byte length (`st_size`).
    pub logical_bytes: u64,
    /// Physical allocated bytes on disk (`st_blocks * 512` on Unix).
    pub allocated_bytes: u64,
    /// Device identifier for same-filesystem enforcement.
    pub device_id: DeviceId,
    /// Filesystem inode or file identifier for hardlink deduplication.
    pub inode: u64,
    /// Hardlink reference count.
    pub link_count: u64,
    /// Last modification timestamp if available.
    pub modified: Option<SystemTime>,
    /// Last access timestamp if available.
    pub accessed: Option<SystemTime>,
    /// Whether the entry is considered hidden by platform conventions.
    pub is_hidden: bool,
}

impl EntryMetadata {
    /// Constructs an `EntryMetadata` record.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "metadata snapshot mirrors OS stat fields directly"
    )]
    pub const fn new(
        kind: EntryKind,
        logical_bytes: u64,
        allocated_bytes: u64,
        device_id: DeviceId,
        inode: u64,
        link_count: u64,
        modified: Option<SystemTime>,
        accessed: Option<SystemTime>,
        is_hidden: bool,
    ) -> Self {
        Self {
            kind,
            logical_bytes,
            allocated_bytes,
            device_id,
            inode,
            link_count,
            modified,
            accessed,
            is_hidden,
        }
    }
}

/// Capacity and capability summary for a mounted storage volume.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VolumeInfo {
    /// Human-friendly volume display name (e.g. `"Macintosh HD"`).
    pub name: String,
    /// Mount point path on the host OS.
    pub mount_point: PathBuf,
    /// Total capacity in bytes.
    pub total_bytes: u64,
    /// Used bytes across the volume.
    pub used_bytes: u64,
    /// Available bytes for non-privileged users.
    pub available_bytes: u64,
    /// Filesystem type name (e.g. `"apfs"`, `"ext4"`, `"ntfs"`).
    pub filesystem_type: String,
    /// Whether the volume is removable media.
    pub is_removable: bool,
    /// Whether copy-on-write or SSD wear-leveling limits multi-pass wipe guarantees.
    pub is_cow_or_ssd: bool,
    /// Platform device identifier of this volume.
    pub device_id: DeviceId,
}

impl VolumeInfo {
    /// Constructs a `VolumeInfo` snapshot.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "volume summary carries independent OS filesystem attributes"
    )]
    pub const fn new(
        name: String,
        mount_point: PathBuf,
        total_bytes: u64,
        used_bytes: u64,
        available_bytes: u64,
        filesystem_type: String,
        is_removable: bool,
        is_cow_or_ssd: bool,
        device_id: DeviceId,
    ) -> Self {
        Self {
            name,
            mount_point,
            total_bytes,
            used_bytes,
            available_bytes,
            filesystem_type,
            is_removable,
            is_cow_or_ssd,
            device_id,
        }
    }
}

/// High-level storage categories matching the visual palette and sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum StorageCategory {
    /// Applications and application support bundles.
    Apps,
    /// Developer toolchains, build directories, and source repositories.
    Developer,
    /// Photos and raster/RAW image assets.
    Photos,
    /// Videos and recorded media.
    Movies,
    /// User documents, downloads, archives, and desktop items.
    Documents,
    /// Music and audio assets.
    Music,
    /// OS internals, preferences, and miscellaneous system/library data.
    SystemData,
    /// Application and system caches.
    Caches,
    /// Items residing inside Trash / Recycle Bin folders.
    Trash,
}

impl StorageCategory {
    /// Ordered list of all nine categories shown in the sidebar and dot matrix.
    pub const ALL: [Self; 9] = [
        Self::Apps,
        Self::Developer,
        Self::Photos,
        Self::Movies,
        Self::Documents,
        Self::Music,
        Self::SystemData,
        Self::Caches,
        Self::Trash,
    ];

    /// Zero-based index into `ALL`.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Apps => 0,
            Self::Developer => 1,
            Self::Photos => 2,
            Self::Movies => 3,
            Self::Documents => 4,
            Self::Music => 5,
            Self::SystemData => 6,
            Self::Caches => 7,
            Self::Trash => 8,
        }
    }

    /// Display label matching the UI sidebar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Apps => "Apps",
            Self::Developer => "Developer",
            Self::Photos => "Photos",
            Self::Movies => "Movies",
            Self::Documents => "Documents",
            Self::Music => "Music",
            Self::SystemData => "System data",
            Self::Caches => "Caches",
            Self::Trash => "Trash",
        }
    }
}

/// Reason an entry is flagged as reclaimable junk or unused storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JunkKind {
    /// Application or browser cache data.
    Cache,
    /// Regenerable compiler or package manager build output.
    BuildArtifact,
    /// File or directory untouched for more than 180 days.
    StaleUnused,
    /// Item already sitting in Trash.
    TrashItem,
    /// Leftover installer bundle (`.dmg`, `.pkg`, `.iso`, etc.).
    InstallerPackage,
}

/// Classification outcome indicating whether an entry may be safely reviewed as junk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct JunkAssessment {
    /// Specific junk classification if flagged.
    pub kind: Option<JunkKind>,
    /// Whether the folder warrants manual review before batch deletion.
    pub review_first: bool,
    /// Short human-readable explanation of why the item was flagged.
    pub reason: &'static str,
}

impl JunkAssessment {
    /// Empty assessment when an entry is normal active data.
    pub const CLEAN: Self = Self {
        kind: None,
        review_first: false,
        reason: "",
    };

    /// Constructs a `JunkAssessment`.
    #[must_use]
    pub const fn new(kind: Option<JunkKind>, review_first: bool, reason: &'static str) -> Self {
        Self {
            kind,
            review_first,
            reason,
        }
    }
}

/// Platform-specific guidance displayed when unreadable directories are encountered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AccessHint {
    /// Short banner prompt (e.g. `"grant Full Disk Access"`).
    pub summary: String,
    /// Action button text.
    pub action_label: String,
    /// Optional OS settings deep link.
    pub settings_uri: Option<String>,
}

impl AccessHint {
    /// Constructs an `AccessHint`.
    #[must_use]
    pub const fn new(summary: String, action_label: String, settings_uri: Option<String>) -> Self {
        Self {
            summary,
            action_label,
            settings_uri,
        }
    }
}

/// Reason a path is protected from destructive deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtectionReason {
    /// Filesystem root (`/`, `C:\`).
    SystemRoot,
    /// Core operating system directory (`/System`, `/usr`, `/Windows`, etc.).
    OperatingSystemCore,
    /// The active user's home directory itself.
    UserHomeRoot,
    /// Virtual or kernel-backed mount (`/proc`, `/sys`, `/dev`).
    VirtualFilesystem,
}
