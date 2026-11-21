//! Headless NSH integration test covering scanning, filtering, snagging, and erasure.

use std::fs;
use std::path::PathBuf;
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

static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn make_fake_host() -> Box<dyn PlatformHost> {
    let root = TEST_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Box::new(FakePlatform::new(root))
}

#[test]
fn scans_filters_snags_and_erases_inside_temp_root() {
    let outer = tempfile::tempdir().expect("tempdir");
    let scan_root = outer.path().join("sandbox");
    let cache_dir = scan_root.join("Caches/Browser");
    fs::create_dir_all(&cache_dir).expect("mkdir caches");

    let target_file = cache_dir.join("stale_dump.tmp");
    fs::write(&target_file, vec![0x5a_u8; 8_192]).expect("write cache blob");

    let _ = TEST_ROOT.set(scan_root.clone());
    configure_launch(LaunchConfig::new(Some(scan_root), make_fake_host));

    let mut config = EngineConfig::new(Root::of::<App>());
    config.viewport = Size::new(Points(1240.0), Points(800.0));
    config.environment.os = Os::MacOs;

    let mut app = Headless::start(config).expect("start headless");
    app.wait_for("Caches/").expect("scan tree shows Caches");

    // Toggle junk filter, snag Caches/, confirm MoveToTrash, verify deletion on disk.
    app.press("role:button[Toggle junk and unused filter]")
        .expect("toggle junk filter");
    app.wait_for("stale_dump.tmp").expect("junk file visible");

    app.press("role:button[s snag]").expect("snag item");
    app.wait_for("destruction queue (1)")
        .expect("drawer shows 1 snagged");

    app.press("role:button[Review and destroy snagged items]")
        .expect("open confirm modal");
    app.wait_for("Confirm Storage Destruction")
        .expect("confirm modal visible");

    app.press("role:button[Confirm destruction]")
        .expect("execute destruction");

    app.wait_for_gone("stale_dump.tmp", Duration::from_secs(5))
        .expect("deleted item pruned from UI");
    assert!(!target_file.exists());
}
