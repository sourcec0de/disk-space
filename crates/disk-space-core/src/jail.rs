//! Root-jail confinement enforcing that scans and deletions never escape `ScanRoot`.

use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use disk_space_platform::DeviceId;
use disk_space_platform::EntryKind;
use disk_space_platform::PlatformError;
use disk_space_platform::PlatformHost;
use disk_space_platform::ProtectionReason;
use thiserror::Error;

/// Policy governing whether traversal may cross filesystem mount boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum CrossMountPolicy {
    /// Stay strictly on the root path's filesystem device.
    #[default]
    StayOnRootDevice,
    /// Permit crossing into child mounts (not recommended for deletion workflows).
    AllowCrossDevice,
}

/// Safety-rail errors preventing scans or deletions outside the authorized root.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JailError {
    /// Scan root path does not exist or could not be canonicalized.
    #[error("scan root `{path}` not found: {source}")]
    RootNotFound {
        /// Original path requested by caller.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Scan root exists but is not a directory.
    #[error("scan root `{path}` is not a directory")]
    RootNotDirectory {
        /// Path that was not a directory.
        path: PathBuf,
    },
    /// Candidate path falls outside the active `ScanRoot`.
    #[error("path `{candidate}` escapes scan root `{root}`")]
    EscapesRoot {
        /// Candidate path.
        candidate: PathBuf,
        /// Active scan root.
        root: PathBuf,
    },
    /// Candidate path contains `..` parent traversal segments.
    #[error("path `{candidate}` contains parent traversal (`..`)")]
    ContainsParentTraversal {
        /// Rejected candidate path.
        candidate: PathBuf,
    },
    /// An ancestor or target path is a symbolic link that could redirect outside the jail.
    #[error("path `{path}` traverses or targets a symlink inside jail")]
    SymlinkEscape {
        /// Symlink path detected during validation.
        path: PathBuf,
    },
    /// Attempted to stage or destroy the scan root directory itself.
    #[error("cannot destroy the scan root itself (`{root}`)")]
    CannotDeleteRoot {
        /// Active scan root.
        root: PathBuf,
    },
    /// Attempted to stage or destroy an OS-protected directory.
    #[error("path `{path}` is protected ({reason:?})")]
    ProtectedSystemPath {
        /// Protected path.
        path: PathBuf,
        /// Reason given by [`PlatformHost`].
        reason: ProtectionReason,
    },
    /// Path crosses onto a different device/mount than `ScanRoot`.
    #[error("path `{path}` crosses mount boundary from `{root}`")]
    CrossesMountBoundary {
        /// Path on a different device.
        path: PathBuf,
        /// Active scan root.
        root: PathBuf,
    },
    /// Scan was cancelled before completion.
    #[error("scan cancelled")]
    Cancelled,
    /// Underlying platform query failed.
    #[error("platform error: {0}")]
    Platform(#[from] PlatformError),
}

/// Authorized root directory to which all scanning and destructive actions are confined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRoot {
    canonical_path: PathBuf,
    device_id: DeviceId,
    display_name: String,
}

impl ScanRoot {
    /// Creates and validates a [`ScanRoot`] from a user-supplied path.
    ///
    /// # Errors
    ///
    /// Returns [`JailError::RootNotFound`], [`JailError::RootNotDirectory`], or
    /// [`JailError::Platform`] if `path` cannot be canonicalized or is not a directory.
    pub fn new(path: &Path, host: &dyn PlatformHost) -> Result<Self, JailError> {
        let canonical = fs::canonicalize(path).map_err(|source| JailError::RootNotFound {
            path: path.to_path_buf(),
            source,
        })?;
        let meta = host.lstat(&canonical)?;
        if meta.kind != EntryKind::Directory {
            return Err(JailError::RootNotDirectory { path: canonical });
        }
        let display_name = canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| canonical.display().to_string());

        Ok(Self {
            canonical_path: canonical,
            device_id: meta.device_id,
            display_name,
        })
    }

    /// Canonical absolute path of the scan root.
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Filesystem device identifier of the scan root.
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Short display label for the scan root.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Validates that `candidate` lies strictly within this [`ScanRoot`] without symlink escapes.
    ///
    /// # Errors
    ///
    /// Returns [`JailError`] if `candidate` contains `..`, traverses a symlink, crosses
    /// mount boundaries, or falls outside `self.canonical_path()`.
    pub fn jail_for_read(
        &self,
        candidate: &Path,
        host: &dyn PlatformHost,
    ) -> Result<JailedPath, JailError> {
        self.validate_path(candidate, host, false)
    }

    /// Validates that `candidate` is safe to stage or destroy inside this [`ScanRoot`].
    ///
    /// Rejects the root itself, any `..` traversal, any symlink ancestor, any path outside
    /// `ScanRoot`, any cross-device path, and any OS-protected system path.
    ///
    /// # Errors
    ///
    /// Returns [`JailError`] if any confinement or safety invariant fails.
    pub fn jail_for_destruction(
        &self,
        candidate: &Path,
        host: &dyn PlatformHost,
    ) -> Result<JailedPath, JailError> {
        let jailed = self.validate_path(candidate, host, true)?;
        if jailed.absolute == self.canonical_path {
            return Err(JailError::CannotDeleteRoot {
                root: self.canonical_path.clone(),
            });
        }
        if let Some(reason) = host.protection_reason(&jailed.absolute) {
            return Err(JailError::ProtectedSystemPath {
                path: jailed.absolute,
                reason,
            });
        }
        Ok(jailed)
    }

    fn validate_path(
        &self,
        candidate: &Path,
        host: &dyn PlatformHost,
        allow_leaf_symlink: bool,
    ) -> Result<JailedPath, JailError> {
        for component in candidate.components() {
            if matches!(component, Component::ParentDir) {
                return Err(JailError::ContainsParentTraversal {
                    candidate: candidate.to_path_buf(),
                });
            }
        }

        let resolved_abs = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.canonical_path.join(candidate)
        };

        let relative = resolved_abs
            .strip_prefix(&self.canonical_path)
            .map(Path::to_path_buf)
            .map_err(|_| JailError::EscapesRoot {
                candidate: candidate.to_path_buf(),
                root: self.canonical_path.clone(),
            })?;

        let components: Vec<_> = relative.components().collect();
        let mut cursor = self.canonical_path.clone();

        for (idx, component) in components.iter().enumerate() {
            cursor.push(component.as_os_str());
            let meta = host.lstat(&cursor)?;
            let is_last = idx + 1 == components.len();

            if meta.kind == EntryKind::Symlink && (!is_last || !allow_leaf_symlink) {
                return Err(JailError::SymlinkEscape { path: cursor });
            }
            if meta.kind != EntryKind::Symlink && meta.device_id != self.device_id {
                return Err(JailError::CrossesMountBoundary {
                    path: cursor,
                    root: self.canonical_path.clone(),
                });
            }
        }

        if let Some(parent) = cursor.parent()
            && !cursor.as_os_str().is_empty()
            && cursor != self.canonical_path
        {
            let canonical_parent =
                fs::canonicalize(parent).map_err(|source| PlatformError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            if !canonical_parent.starts_with(&self.canonical_path) {
                return Err(JailError::EscapesRoot {
                    candidate: cursor,
                    root: self.canonical_path.clone(),
                });
            }
        }

        Ok(JailedPath {
            absolute: cursor,
            relative,
        })
    }
}

/// Path proven to reside strictly within a [`ScanRoot`] with no symlink or `..` escape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JailedPath {
    absolute: PathBuf,
    relative: PathBuf,
}

impl JailedPath {
    /// Absolute path guaranteed inside the [`ScanRoot`].
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.absolute
    }

    /// Relative path from the [`ScanRoot`] to this item.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative
    }
}

#[cfg(test)]
mod tests {
    use disk_space_platform::FakePlatform;

    use super::*;

    #[test]
    fn rejects_root_escape_traversal_and_symlinks() {
        let outer = tempfile::tempdir().expect("outer tempdir");
        let root_dir = outer.path().join("sandbox");
        let outside_dir = outer.path().join("outside");
        fs::create_dir_all(root_dir.join("sub")).expect("create sub");
        fs::create_dir_all(&outside_dir).expect("create outside");
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, b"keep me").expect("write secret");
        let safe_file = root_dir.join("sub/cache.bin");
        fs::write(&safe_file, b"delete me").expect("write safe");

        let host = FakePlatform::new(root_dir.clone());
        let scan_root = ScanRoot::new(&root_dir, &host).expect("scan root");

        // Reject root deletion itself.
        assert!(matches!(
            scan_root.jail_for_destruction(&root_dir, &host),
            Err(JailError::CannotDeleteRoot { .. })
        ));

        // Reject `..` traversal.
        assert!(matches!(
            scan_root.jail_for_destruction(&root_dir.join("sub/../../outside/secret.txt"), &host),
            Err(JailError::ContainsParentTraversal { .. })
        ));

        // Reject absolute path outside root.
        assert!(matches!(
            scan_root.jail_for_destruction(&outside_file, &host),
            Err(JailError::EscapesRoot { .. })
        ));

        // Reject path traversing through an internal symlink pointing outside.
        #[cfg(unix)]
        {
            let link_dir = root_dir.join("escape_link");
            std::os::unix::fs::symlink(&outside_dir, &link_dir).expect("symlink dir");
            assert!(matches!(
                scan_root.jail_for_destruction(&link_dir.join("secret.txt"), &host),
                Err(JailError::SymlinkEscape { .. })
            ));
        };

        // Accept legitimate file inside jail.
        let jailed = scan_root
            .jail_for_destruction(&safe_file, &host)
            .expect("valid child");
        assert_eq!(jailed.relative_path(), Path::new("sub/cache.bin"));
    }

    #[test]
    fn rejects_protected_path_and_cross_device_path() {
        let outer = tempfile::tempdir().expect("outer tempdir");
        let root_dir = outer.path().join("sandbox");
        let protected = root_dir.join("critical");
        let mount_child = root_dir.join("external_mount");
        fs::create_dir_all(&protected).expect("create protected");
        fs::create_dir_all(&mount_child).expect("create mount");

        let host = FakePlatform::new(root_dir.clone()).with_protected_path(protected.clone());
        host.set_device_override(mount_child.clone(), DeviceId::new(999_999));

        let scan_root = ScanRoot::new(&root_dir, &host).expect("scan root");
        assert!(matches!(
            scan_root.jail_for_destruction(&protected, &host),
            Err(JailError::ProtectedSystemPath { .. })
        ));
        assert!(matches!(
            scan_root.jail_for_destruction(&mount_child, &host),
            Err(JailError::CrossesMountBoundary { .. })
        ));
    }
}
