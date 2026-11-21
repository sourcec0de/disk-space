//! Native desktop launcher for `disk-space`.

#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::process::Termination;

use disk_space_platform::native_platform;
use disk_space_ui::App;
use disk_space_ui::LaunchConfig;
use disk_space_ui::configure_launch;
use nsh_core::platform::Appearance;
#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;
#[cfg(target_os = "windows")]
use nsh_platform_windows as platform;

fn parse_args() -> Result<Option<PathBuf>, ExitCode> {
    let mut args = env::args_os().skip(1);
    let mut root: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "-h" | "--help" => {
                println!(
                    "disk-space {}\n\nUsage:\n  disk-space-app [--root <PATH>]\n\nOptions:\n  --root <PATH>  Initial scan root directory (defaults to $HOME)\n  -h, --help     Print help\n  -V, --version  Print version",
                    env!("CARGO_PKG_VERSION")
                );
                return Err(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!("disk-space {}", env!("CARGO_PKG_VERSION"));
                return Err(ExitCode::SUCCESS);
            }
            "--root" => {
                let Some(val) = args.next() else {
                    eprintln!("error: `--root` requires a directory path");
                    return Err(ExitCode::FAILURE);
                };
                root = Some(PathBuf::from(val));
            }
            other => {
                if !other.starts_with('-') && root.is_none() {
                    root = Some(PathBuf::from(arg));
                } else {
                    eprintln!("error: unknown argument `{other}` (use `--help`)");
                    return Err(ExitCode::FAILURE);
                }
            }
        }
    }
    Ok(root)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn main() -> impl Termination {
    env_logger::init();
    let initial_root = match parse_args() {
        Ok(root) => root,
        Err(code) => return code,
    };

    configure_launch(LaunchConfig::new(initial_root, native_platform));

    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<App>()),
        platform::AppOptions {
            title: "disk-space".to_owned(),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            app_id: "dev.sourcec0de.disk_space".to_owned(),
            size: (1240, 800),
            appearance: Appearance::Dark,
            ..platform::AppOptions::default()
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("disk-space desktop launcher requires macOS, Linux, or Windows.");
