//! Shared POSIX metadata inspection and `O_NOFOLLOW` file-opening helpers.

#![cfg(unix)]

use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use crate::error::PlatformError;
use crate::types::DeviceId;
use crate::types::EntryKind;
use crate::types::EntryMetadata;

/// Inspects `path` via `lstat` without following symbolic links.
pub(crate) fn lstat_unix(path: &Path) -> Result<EntryMetadata, PlatformError> {
    let meta = fs::symlink_metadata(path).map_err(|source| map_io_error(path, source))?;
    let kind = entry_kind_from_mode(meta.mode());
    let is_hidden = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'));

    let allocated_bytes = if kind == EntryKind::Symlink {
        0
    } else {
        meta.blocks().saturating_mul(512)
    };

    Ok(EntryMetadata::new(
        kind,
        meta.len(),
        allocated_bytes,
        DeviceId::new(meta.dev()),
        meta.ino(),
        meta.nlink(),
        meta.modified().ok(),
        meta.accessed().ok(),
        is_hidden,
    ))
}

/// Opens an existing regular file with `O_NOFOLLOW` for read/write overwriting.
pub(crate) fn open_regular_nofollow_rw_unix(path: &Path) -> Result<File, PlatformError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| map_io_error(path, source))?;

    let meta = file
        .metadata()
        .map_err(|source| map_io_error(path, source))?;
    if entry_kind_from_mode(meta.mode()) != EntryKind::File {
        return Err(PlatformError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "target is not a regular file"),
        });
    }
    Ok(file)
}

pub(crate) fn map_io_error(path: &Path, source: io::Error) -> PlatformError {
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

const fn entry_kind_from_mode(mode: u32) -> EntryKind {
    // Compare POSIX file-type bits directly instead of calling `FileType::is_file`.
    let file_type = mode & (libc::S_IFMT as u32);
    if file_type == (libc::S_IFREG as u32) {
        EntryKind::File
    } else if file_type == (libc::S_IFDIR as u32) {
        EntryKind::Directory
    } else if file_type == (libc::S_IFLNK as u32) {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}
