//! Headless NSH integration test covering search-to-list focus jump, Shift multi-select, bulk snag, and hover snag.

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

static KEYBOARD_TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn make_fake_host() -> Box<dyn PlatformHost> {
    let root = KEYBOARD_TEST_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Box::new(FakePlatform::new(root))
}

#[test]
fn search_down_arrow_enters_list_shift_selects_both_directions_and_hover_snags() {
    let outer = tempfile::tempdir().expect("tempdir");
    let scan_root = outer.path().join("downloads");
    fs::create_dir_all(&scan_root).expect("mkdir downloads");

    fs::write(scan_root.join("beta_other.txt"), vec![1_u8; 98_304]).expect("write beta");
    fs::write(scan_root.join("alpha_one.log"), vec![2_u8; 81_920]).expect("write alpha 1");
    fs::write(scan_root.join("alpha_two.log"), vec![3_u8; 65_536]).expect("write alpha 2");
    fs::write(scan_root.join("alpha_three.log"), vec![4_u8; 49_152]).expect("write alpha 3");
    fs::write(scan_root.join("alpha_four.log"), vec![5_u8; 32_768]).expect("write alpha 4");
    fs::write(scan_root.join("alpha_five.log"), vec![6_u8; 16_384]).expect("write alpha 5");

    let _ = KEYBOARD_TEST_ROOT.set(scan_root.clone());
    configure_launch(LaunchConfig::new(Some(scan_root), make_fake_host));

    let mut config = EngineConfig::new(Root::of::<App>());
    config.viewport = Size::new(Points(1240.0), Points(800.0));
    config.environment.os = Os::MacOs;

    let mut app = Headless::start(config).expect("start headless");
    app.wait_for("beta_other.txt")
        .expect("initial listing shows beta_other.txt");

    // Type search query, then press Down to leave the input and focus the list.
    app.type_text("label:Search storage", "alpha")
        .expect("type search query");
    app.wait_for("alpha_one.log")
        .expect("filtered listing shows alpha_one.log");
    app.wait_for("alpha_five.log")
        .expect("filtered listing shows alpha_five.log");
    assert!(!app.screen().shows("beta_other.txt"));

    app.press_key("down")
        .expect("down arrow jumps from search input to list");

    // Extend downward with Shift+Down, retract with Shift+Up, then bulk snag with `s`.
    app.press_key("shift+down")
        .expect("shift+down selects first two items");
    app.wait_for("2 selected")
        .expect("selection summary shows 2 selected");

    app.press_key("shift+down")
        .expect("shift+down extends selection to 3 items");
    app.wait_for("3 selected")
        .expect("selection summary shows 3 selected");

    app.press_key("shift+up")
        .expect("shift+up retracts selection back to 2 items");
    app.wait_for("2 selected")
        .expect("selection summary shows 2 selected after retract");

    app.press_key("s").expect("s snags all selected items");
    app.wait_for("destruction queue (2)")
        .expect("destruction queue holds both snagged items");
    app.wait_for("3 items")
        .expect("explorer list drops from 5 to 3 items after exit animation");
    app.wait_for_gone("role:button[Snag alpha_one.log]", Duration::from_secs(3))
        .expect("alpha_one.log row animates out of view");
    app.wait_for_gone("role:button[Snag alpha_two.log]", Duration::from_secs(3))
        .expect("alpha_two.log row animates out of view");
    assert!(app.screen().shows("alpha_three.log"));

    // Move down to alpha_five.log, select upward with Shift+Up, and snag with Shift+S.
    app.press_key("down").expect("move to alpha_four.log");
    app.press_key("down").expect("move to alpha_five.log");
    app.press_key("shift+up")
        .expect("shift+up selects alpha_four.log and alpha_five.log");
    app.wait_for("2 selected")
        .expect("upward shift selection shows 2 selected");

    app.press_key("shift+s")
        .expect("shift+s snags upward-selected items");
    app.wait_for("destruction queue (4)")
        .expect("destruction queue holds 4 items");
    app.wait_for("1 items")
        .expect("only alpha_three.log remains in filtered list");
    app.wait_for_gone("role:button[Snag alpha_four.log]", Duration::from_secs(3))
        .expect("alpha_four.log row animates out");
    app.wait_for_gone("role:button[Snag alpha_five.log]", Duration::from_secs(3))
        .expect("alpha_five.log row animates out");

    // Clear search: cursor lands on beta_other.txt; hover alpha_three.log and snag without clicking row first.
    app.press("role:button[Clear search]")
        .expect("clear search query");
    app.wait_for("beta_other.txt")
        .expect("beta_other.txt visible again");
    app.wait_for("alpha_three.log")
        .expect("alpha_three.log still visible");

    app.hover("alpha_three.log")
        .expect("hover non-selected row alpha_three.log");
    app.press("role:button[Snag alpha_three.log]")
        .expect("press hover snag button on non-selected row");
    app.wait_for("destruction queue (5)")
        .expect("hover snag adds 5th item to queue");
    app.wait_for("1 items")
        .expect("explorer list drops to only beta_other.txt");
    app.wait_for_gone("role:button[Snag alpha_three.log]", Duration::from_secs(3))
        .expect("hover-snagged row animates out");
    assert!(app.screen().shows("beta_other.txt"));
}
