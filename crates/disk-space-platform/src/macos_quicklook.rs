//! Native macOS Quick Look (`QLPreviewPanel`) integration.
//!
//! Uses `QuickLookUI.framework`'s `QLPreviewPanel` directly in-process instead of
//! spawning Apple's debug CLI `qlmanage -p`, avoiding `AVKit` crashes on media files
//! and matching Finder's exact Spacebar preview behavior.

#![expect(
    unsafe_code,
    reason = "macOS QuickLookUI.framework QLPreviewPanel requires Objective-C FFI"
)]

use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;

use objc2::DefinedClass;
use objc2::MainThreadMarker;
use objc2::MainThreadOnly;
use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2::runtime::AnyObject;
use objc2_foundation::NSObject;
use objc2_foundation::NSString;
use objc2_foundation::NSURL;

use crate::error::PlatformError;

#[link(name = "QuickLookUI", kind = "framework")]
unsafe extern "C" {}

#[derive(Default)]
struct QuickLookIvars {
    path: RefCell<Option<PathBuf>>,
    url: RefCell<Option<Retained<NSURL>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = QuickLookIvars]
    struct DiskSpaceQuickLookDataSource;

    impl DiskSpaceQuickLookDataSource {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn number_of_preview_items(&self, _panel: &AnyObject) -> isize {
            isize::from(self.ivars().url.borrow().is_some())
        }

        #[unsafe(method(previewPanel:previewItemAtIndex:))]
        fn preview_item_at_index(&self, _panel: &AnyObject, index: isize) -> *const AnyObject {
            if index != 0 {
                return ptr::null();
            }
            self.ivars()
                .url
                .borrow()
                .as_ref()
                .map_or(ptr::null(), |url| Retained::as_ptr(url).cast::<AnyObject>())
        }
    }
);

impl DiskSpaceQuickLookDataSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(QuickLookIvars::default());
        // SAFETY: `NSObject`'s `init` is valid on a freshly allocated subclass instance.
        unsafe { msg_send![super(this), init] }
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.ivars().path.borrow().clone()
    }

    fn set_path(&self, path: &Path) -> Result<(), PlatformError> {
        let path_str = path.to_str().ok_or_else(|| PlatformError::CommandFailed {
            command: "QLPreviewPanel",
            message: format!("path is not valid UTF-8: {}", path.display()),
        })?;
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);
        *self.ivars().path.borrow_mut() = Some(path.to_path_buf());
        *self.ivars().url.borrow_mut() = Some(url);
        Ok(())
    }
}

thread_local! {
    static DATA_SOURCE: RefCell<Option<Retained<DiskSpaceQuickLookDataSource>>> =
        const { RefCell::new(None) };
}

/// Toggles macOS Quick Look (`QLPreviewPanel`) for `path`.
///
/// If Quick Look is already visible for the same `path`, closes the panel.
/// Otherwise opens or updates the shared panel to preview `path`.
pub(crate) fn toggle_quick_look(path: &Path) -> Result<(), PlatformError> {
    let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CommandFailed {
        command: "QLPreviewPanel",
        message: "Quick Look must be invoked on the main thread".to_owned(),
    })?;

    let data_source = DATA_SOURCE.with(|slot| {
        let mut borrow = slot.borrow_mut();
        borrow
            .get_or_insert_with(|| DiskSpaceQuickLookDataSource::new(mtm))
            .clone()
    });

    let panel_cls =
        AnyClass::get(c"QLPreviewPanel").ok_or_else(|| PlatformError::CommandFailed {
            command: "QLPreviewPanel",
            message: "QuickLookUI.framework QLPreviewPanel class unavailable".to_owned(),
        })?;

    // SAFETY: `QLPreviewPanel.sharedPreviewPanel` is a standard AppKit/QuickLookUI class
    // method safe to call on the main thread, returning the shared `QLPreviewPanel` instance.
    let panel: Option<Retained<AnyObject>> = unsafe { msg_send![panel_cls, sharedPreviewPanel] };
    let panel = panel.ok_or_else(|| PlatformError::CommandFailed {
        command: "QLPreviewPanel",
        message: "sharedPreviewPanel returned nil".to_owned(),
    })?;

    // SAFETY: `QLPreviewPanel` inherits `NSWindow::isVisible`, returning a `BOOL`.
    let visible: bool = unsafe { msg_send![&*panel, isVisible] };

    if visible && data_source.current_path().as_deref() == Some(path) {
        let nil: *const AnyObject = ptr::null();
        // SAFETY: `orderOut:` accepts a nullable sender pointer on the main thread.
        let (): () = unsafe { msg_send![&*panel, orderOut: nil] };
        return Ok(());
    }

    data_source.set_path(path)?;

    let ds_ref: &AnyObject = &data_source;
    // SAFETY: `data_source` implements `QLPreviewPanelDataSource` and stays retained in `DATA_SOURCE`.
    let (): () = unsafe { msg_send![&*panel, setDataSource: ds_ref] };
    // SAFETY: `reloadData` refreshes `QLPreviewPanel` from its data source on the main thread.
    let (): () = unsafe { msg_send![&*panel, reloadData] };

    if !visible {
        let nil: *const AnyObject = ptr::null();
        // SAFETY: `makeKeyAndOrderFront:` presents the panel on the main thread.
        let (): () = unsafe { msg_send![&*panel, makeKeyAndOrderFront: nil] };
        // Re-assert `dataSource` and reload after `makeKeyAndOrderFront:` in case AppKit's
        // responder-chain `updateController` cleared it while activating the panel.
        // SAFETY: `data_source` remains retained in `DATA_SOURCE` for the panel's lifetime.
        let (): () = unsafe { msg_send![&*panel, setDataSource: ds_ref] };
        // SAFETY: `reloadData` is safe to invoke on the main thread.
        let (): () = unsafe { msg_send![&*panel, reloadData] };
    }

    Ok(())
}
