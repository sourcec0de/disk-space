//! Headless NSH integration test covering back/forward navigation, Quick Look spacebar preview,
//! and live keybinding inspection & rebinding in the Settings modal.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use disk_space_platform::FakePlatform;
use disk_space_platform::PlatformHost;
use disk_space_ui::App;
use disk_space_ui::LaunchConfig;
use disk_space_ui::configure_launch;
use nsh_core::Points;
use nsh_core::Size;
use nsh_core::platform::Os;
use nsh_engine::EngineConfig;
use nsh_engine::Root;
use nsh_testing::Headless;

static PREVIEW_TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();
static PREVIEW_LOG: OnceLock<Arc<Mutex<Vec<PathBuf>>>> = OnceLock::new();

#[derive(Debug)]
struct RecordingFakeHost {
    inner: FakePlatform,
    preview_log: Arc<Mutex<Vec<PathBuf>>>,
}

impl disk_space_platform::VolumeProvider for RecordingFakeHost {
    fn list_volumes(
        &self,
    ) -> Result<Vec<disk_space_platform::VolumeInfo>, disk_space_platform::PlatformError> {
        self.inner.list_volumes()
    }

    fn volume_for_path(
        &self,
        path: &std::path::Path,
    ) -> Result<disk_space_platform::VolumeInfo, disk_space_platform::PlatformError> {
        self.inner.volume_for_path(path)
    }
}

impl disk_space_platform::FileMetadataInspector for RecordingFakeHost {
    fn lstat(
        &self,
        path: &std::path::Path,
    ) -> Result<disk_space_platform::EntryMetadata, disk_space_platform::PlatformError> {
        self.inner.lstat(path)
    }

    fn open_regular_nofollow_rw(
        &self,
        path: &std::path::Path,
    ) -> Result<fs::File, disk_space_platform::PlatformError> {
        self.inner.open_regular_nofollow_rw(path)
    }
}

impl disk_space_platform::PermissionAdvisor for RecordingFakeHost {
    fn access_hint(&self, unreadable_count: usize) -> Option<disk_space_platform::AccessHint> {
        self.inner.access_hint(unreadable_count)
    }

    fn open_permission_settings(&self) -> Result<(), disk_space_platform::PlatformError> {
        self.inner.open_permission_settings()
    }
}

impl disk_space_platform::CategoryClassifier for RecordingFakeHost {
    fn classify(
        &self,
        path: &std::path::Path,
        kind: disk_space_platform::EntryKind,
        metadata: &disk_space_platform::EntryMetadata,
        now: std::time::SystemTime,
    ) -> (
        disk_space_platform::StorageCategory,
        disk_space_platform::JunkAssessment,
    ) {
        self.inner.classify(path, kind, metadata, now)
    }
}

impl disk_space_platform::ProtectedPathPolicy for RecordingFakeHost {
    fn protection_reason(
        &self,
        canonical_path: &std::path::Path,
    ) -> Option<disk_space_platform::ProtectionReason> {
        self.inner.protection_reason(canonical_path)
    }

    fn should_skip_scan_entry(&self, path: &std::path::Path, scan_root: &std::path::Path) -> bool {
        self.inner.should_skip_scan_entry(path, scan_root)
    }
}

impl disk_space_platform::FileActionBackend for RecordingFakeHost {
    fn reveal_in_file_manager(
        &self,
        path: &std::path::Path,
    ) -> Result<(), disk_space_platform::PlatformError> {
        self.inner.reveal_in_file_manager(path)
    }

    fn open_item(&self, path: &std::path::Path) -> Result<(), disk_space_platform::PlatformError> {
        self.inner.open_item(path)
    }

    fn preview_item(
        &self,
        path: &std::path::Path,
    ) -> Result<(), disk_space_platform::PlatformError> {
        if let Ok(mut guard) = self.preview_log.lock() {
            guard.push(path.to_path_buf());
        }
        self.inner.preview_item(path)
    }

    fn move_to_trash(
        &self,
        path: &std::path::Path,
    ) -> Result<(), disk_space_platform::PlatformError> {
        self.inner.move_to_trash(path)
    }
}

fn make_recording_host() -> Box<dyn PlatformHost> {
    let root = PREVIEW_TEST_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let log = Arc::clone(PREVIEW_LOG.get_or_init(|| Arc::new(Mutex::new(Vec::new()))));
    Box::new(RecordingFakeHost {
        inner: FakePlatform::new(root),
        preview_log: log,
    })
}

fn previewed_count() -> usize {
    PREVIEW_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .map_or(0, |guard| guard.len())
}

fn last_previewed_name() -> Option<String> {
    PREVIEW_LOG
        .get()
        .and_then(|log| log.lock().ok())
        .and_then(|guard| {
            guard
                .last()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
}

#[test]
fn nav_history_quick_look_spacebar_and_settings_rebind_work_end_to_end() {
    let outer = tempfile::tempdir().expect("tempdir");
    let scan_root = outer.path().join("workspace");
    let docs_dir = scan_root.join("docs");
    fs::create_dir_all(&docs_dir).expect("mkdir docs");
    fs::write(docs_dir.join("report.pdf"), vec![1_u8; 65_536]).expect("write report");
    fs::write(scan_root.join("notes.txt"), vec![2_u8; 4_096]).expect("write notes");

    let _ = PREVIEW_TEST_ROOT.set(scan_root.clone());
    let _ = PREVIEW_LOG.set(Arc::new(Mutex::new(Vec::new())));
    configure_launch(LaunchConfig::new(Some(scan_root), make_recording_host));

    let mut config = EngineConfig::new(Root::of::<App>());
    config.viewport = Size::new(Points(1240.0), Points(800.0));
    // `Headless` delivers raw `on_key_down` / `KeyCapture::All` events on `Os::Linux`.
    config.environment.os = Os::Linux;

    let mut app = Headless::start(config).expect("start headless");
    app.wait_for("docs/").expect("root shows docs/");
    app.wait_for("notes.txt").expect("root shows notes.txt");

    // Enter docs/ (heaviest first item), then navigate back with mod+[ and forward with mod+].
    app.press_key("enter").expect("enter docs/");
    app.wait_for("report.pdf")
        .expect("inside docs/, report.pdf is visible");
    assert!(!app.screen().shows("notes.txt"));

    app.press_key("mod+[").expect("navigate back with mod+[");
    app.wait_for("notes.txt")
        .expect("back at root, notes.txt is visible");

    app.press_key("mod+]").expect("navigate forward with mod+]");
    app.wait_for("report.pdf")
        .expect("forward into docs/, report.pdf is visible again");

    // Press spacebar on selected report.pdf to trigger Quick Look preview.
    app.press_key("space")
        .expect("space triggers Quick Look preview");
    assert_eq!(previewed_count(), 1);
    assert_eq!(last_previewed_name().as_deref(), Some("report.pdf"));

    // Open settings modal with mod+,, inspect shortcuts, rebind Quick Look to `p`, and test `p`.
    app.press_key("mod+,")
        .expect("mod+, opens shortcut settings");
    app.wait_for("Keyboard & Mouse Shortcuts")
        .expect("settings modal visible");
    assert!(app.screen().shows("Navigate Back"));
    assert!(app.screen().shows("Mouse Back"));
    assert!(app.screen().shows("Quick Look Preview"));

    app.press("role:button[Rebind Quick Look Preview]")
        .expect("start rebinding Quick Look Preview");
    app.wait_for("Rebinding `Quick Look Preview`...")
        .expect("capture banner visible");
    app.press_key("p").expect("record `p` for Quick Look");
    app.wait_for_gone("Rebinding `Quick Look Preview`...", Duration::from_secs(3))
        .expect("recording banner closes after capture");

    app.press_key("escape")
        .expect("escape closes settings modal");
    app.wait_for_gone("Keyboard & Mouse Shortcuts", Duration::from_secs(3))
        .expect("settings modal dismissed");

    // Old `space` binding no longer previews; new `p` binding previews report.pdf.
    app.press_key("space").expect("space is now unbound");
    assert_eq!(previewed_count(), 1);

    app.press_key("p")
        .expect("rebound `p` triggers Quick Look preview");
    assert_eq!(previewed_count(), 2);
    assert_eq!(last_previewed_name().as_deref(), Some("report.pdf"));

    // Snag report.pdf, open confirmation with mod+enter, and confirm directly with enter.
    app.press_key("s").expect("snag report.pdf");
    app.wait_for("destruction queue (1)")
        .expect("report.pdf added to snag queue");
    app.press_key("mod+enter")
        .expect("mod+enter opens destruction confirmation");
    app.wait_for("Confirm Storage Destruction")
        .expect("confirmation dialog visible");
    app.press_key("enter")
        .expect("enter confirms destruction immediately");
    app.wait_for_gone("Confirm Storage Destruction", Duration::from_secs(3))
        .expect("confirmation dialog closes after execution");
    app.wait_for("Snagged (0)")
        .expect("snag queue emptied after execution");
}
