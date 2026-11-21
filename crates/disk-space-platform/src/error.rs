//! Error types returned by platform harness operations.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Errors returned by OS-level volume, metadata, permission, or file actions.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// Underlying filesystem I/O failure for a path.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Path involved in the failing operation.
        path: PathBuf,
        /// Source I/O error.
        #[source]
        source: io::Error,
    },
    /// Access to a path was denied by the operating system.
    #[error("permission denied at {path}")]
    PermissionDenied {
        /// Path that could not be accessed.
        path: PathBuf,
    },
    /// Requested platform operation is not supported on this target.
    #[error("unsupported platform operation: {operation}")]
    Unsupported {
        /// Name of the unsupported operation.
        operation: &'static str,
    },
    /// Moving an item to the system trash failed.
    #[error("failed to move {path} to trash: {message}")]
    TrashFailed {
        /// Path targeted for trash.
        path: PathBuf,
        /// Error explanation from the trash backend.
        message: String,
    },
    /// External OS command invocation failed.
    #[error("command `{command}` failed: {message}")]
    CommandFailed {
        /// Executable name.
        command: &'static str,
        /// Failure details.
        message: String,
    },
}
