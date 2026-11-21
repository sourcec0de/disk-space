//! Headless NSH integration test covering name-focused search and snag pop-out removal.

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

static SEARCH_TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn make_fake_host() -> Box<dyn PlatformHost> {
    let root = SEARCH_TEST_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Box::new(FakePlatform::new(root))
}

#[test]
fn search_hides_non_matching_descendants_and_snagged_rows_pop_out() {
    let outer = tempfile::tempdir().expect("tempdir");
    let scan_root = outer.path().join("downloads");
    let report_data = scan_root.join("playwright-report-core/data");
    let notes_dir = scan_root.join("notes");
    fs::create_dir_all(&report_data).expect("mkdir report_data");
    fs::create_dir_all(&notes_dir).expect("mkdir notes");

    fs::write(report_data.join("8a4f9c2e.webm"), vec![1_u8; 8_192]).expect("write webm");
    fs::write(
        scan_root.join("playwright-report-core/playwright-trace.zip"),
        vec![2_u8; 4_096],
    )
    .expect("write trace");
    fs::write(notes_dir.join("unrelated.txt"), vec![3_u8; 2_048]).expect("write notes");

    let _ = SEARCH_TEST_ROOT.set(scan_root.clone());
    configure_launch(LaunchConfig::new(Some(scan_root), make_fake_host));

    let mut config = EngineConfig::new(Root::of::<App>());
    config.viewport = Size::new(Points(1240.0), Points(800.0));
    config.environment.os = Os::MacOs;

    let mut app = Headless::start(config).expect("start headless");
    app.wait_for("playwright-report-core/")
        .expect("scan tree shows playwright-report-core");

    app.type_text("label:Search storage", "playwright")
        .expect("type search query");
    app.wait_for("playwright-trace.zip")
        .expect("matching nested file visible in search results");
    assert!(app.screen().shows("playwright-report-core/"));
    assert!(!app.screen().shows("8a4f9c2e.webm"));
    assert!(!app.screen().shows("unrelated.txt"));

    app.press("role:button[s snag]")
        .expect("snag top search hit");
    app.wait_for("destruction queue (1)")
        .expect("drawer shows snagged folder");

    app.wait_for_gone("playwright-report-core/", Duration::from_secs(3))
        .expect("snagged folder removed from search results after animation");
    app.wait_for_gone("playwright-trace.zip", Duration::from_secs(3))
        .expect("covered nested match removed from search results after animation");
    assert!(!app.screen().shows("s unsnag"));

    app.press("role:button[Unstage item]")
        .expect("unstage from destruction queue");
    app.wait_for("playwright-report-core/")
        .expect("unstaged folder restored to search results");
}
