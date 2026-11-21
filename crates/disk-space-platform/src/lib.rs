//! Cross-platform operating system harness for `disk-space`.
//!
//! Defines OS-agnostic traits for volumes, file metadata inspection, permission
//! guidance, storage classification, protected path policies, and desktop file
//! actions, along with native backends (macOS, Linux, Windows) and a test fake.

mod classify;
pub mod error;
pub mod fake;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod native;
pub mod traits;
pub mod types;
#[cfg(unix)]
mod unix;
#[cfg(target_os = "windows")]
pub mod windows;

pub use error::PlatformError;
pub use fake::FakePlatform;
#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform;
#[cfg(target_os = "macos")]
pub use macos::MacOsPlatform;
pub use native::native_platform;
pub use traits::CategoryClassifier;
pub use traits::FileActionBackend;
pub use traits::FileMetadataInspector;
pub use traits::PermissionAdvisor;
pub use traits::PlatformHost;
pub use traits::ProtectedPathPolicy;
pub use traits::VolumeProvider;
pub use types::AccessHint;
pub use types::DeviceId;
pub use types::EntryKind;
pub use types::EntryMetadata;
pub use types::JunkAssessment;
pub use types::JunkKind;
pub use types::ProtectionReason;
pub use types::StorageCategory;
pub use types::VolumeInfo;
#[cfg(target_os = "windows")]
pub use windows::WindowsPlatform;
