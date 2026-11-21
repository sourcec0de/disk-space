//! Platform host constructor dispatching to the active operating system backend.

use crate::traits::PlatformHost;

/// Instantiates the native [`PlatformHost`] for the current target OS.
#[must_use]
pub fn native_platform() -> Box<dyn PlatformHost> {
    #[cfg(target_os = "macos")]
    {
        Box::new(crate::macos::MacOsPlatform::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(crate::linux::LinuxPlatform::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(crate::windows::WindowsPlatform::new())
    }
}
