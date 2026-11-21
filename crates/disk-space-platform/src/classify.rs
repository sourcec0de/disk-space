//! Shared cross-platform storage category and junk classification heuristics.

use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;

use crate::types::EntryKind;
use crate::types::EntryMetadata;
use crate::types::JunkAssessment;
use crate::types::JunkKind;
use crate::types::StorageCategory;

/// Threshold (180 days) after which untouched files/folders are flagged as stale/unused.
const STALE_AGE: Duration = Duration::from_hours(180 * 24);

/// Classifies an entry using cross-platform directory, filename, extension, and age rules.
#[must_use]
pub(crate) fn classify_entry(
    path: &Path,
    kind: EntryKind,
    metadata: &EntryMetadata,
    now: SystemTime,
) -> (StorageCategory, JunkAssessment) {
    let lower_path = path.to_string_lossy().to_ascii_lowercase();
    let lower_as_path = Path::new(&lower_path);
    let file_name = lower_as_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");
    let ext = lower_as_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");

    let is_stale = is_entry_stale(metadata, now);

    if is_trash_path(&lower_path) {
        return (
            StorageCategory::Trash,
            JunkAssessment::new(Some(JunkKind::TrashItem), false, "already in Trash"),
        );
    }

    if is_cache_path(&lower_path, file_name) {
        return (
            StorageCategory::Caches,
            JunkAssessment::new(Some(JunkKind::Cache), false, "regenerable cache data"),
        );
    }

    if let Some((category, assessment)) =
        classify_by_location(&lower_path, file_name, ext, kind, is_stale)
    {
        return (category, assessment);
    }

    if kind != EntryKind::Directory
        && let Some((category, assessment)) = classify_by_extension(ext, is_stale)
    {
        return (category, assessment);
    }

    if is_system_path(&lower_path) {
        let assessment = if is_stale {
            JunkAssessment::new(
                Some(JunkKind::StaleUnused),
                true,
                "untouched for over 6 months",
            )
        } else {
            JunkAssessment::CLEAN
        };
        return (StorageCategory::SystemData, assessment);
    }

    let fallback_assessment = if is_stale {
        JunkAssessment::new(
            Some(JunkKind::StaleUnused),
            true,
            "untouched for over 6 months",
        )
    } else {
        JunkAssessment::CLEAN
    };
    (StorageCategory::Documents, fallback_assessment)
}

fn is_entry_stale(metadata: &EntryMetadata, now: SystemTime) -> bool {
    let reference = metadata.accessed.or(metadata.modified);
    let Some(timestamp) = reference else {
        return false;
    };
    now.duration_since(timestamp)
        .is_ok_and(|age| age >= STALE_AGE)
}

fn is_trash_path(lower_path: &str) -> bool {
    lower_path.contains("/.trash/")
        || lower_path.ends_with("/.trash")
        || lower_path.contains("/.trashes/")
        || lower_path.contains("/.local/share/trash/")
        || lower_path.contains("\\$recycle.bin\\")
}

fn is_cache_path(lower_path: &str, file_name: &str) -> bool {
    const CACHE_SEGMENTS: [&str; 11] = [
        "/library/caches/",
        "/library/caches",
        "/.cache/",
        "/cachestorage/",
        "/code cache/",
        "/gpucache/",
        "/dawncache/",
        "/shadercache/",
        "/crx_cache/",
        "\\appdata\\local\\temp\\",
        "/private/var/folders/",
    ];
    if CACHE_SEGMENTS
        .iter()
        .any(|segment| lower_path.contains(segment))
    {
        return true;
    }
    matches!(
        file_name,
        "caches" | "cache" | "cachestorage" | "code cache" | "gpucache" | "crx_cache"
    )
}

fn classify_by_location(
    lower_path: &str,
    file_name: &str,
    ext: &str,
    kind: EntryKind,
    is_stale: bool,
) -> Option<(StorageCategory, JunkAssessment)> {
    if is_build_artifact_name(file_name) || is_developer_path(lower_path) {
        let assessment =
            if is_build_artifact_name(file_name) || lower_path.contains("/deriveddata/") {
                JunkAssessment::new(
                    Some(JunkKind::BuildArtifact),
                    false,
                    "regenerable build or dependency output",
                )
            } else if is_stale {
                JunkAssessment::new(
                    Some(JunkKind::StaleUnused),
                    true,
                    "developer artifact untouched > 6 months",
                )
            } else {
                JunkAssessment::CLEAN
            };
        return Some((StorageCategory::Developer, assessment));
    }

    if is_apps_path(lower_path, ext, kind) {
        let needs_review = lower_path.contains("/application support/");
        let assessment = if is_stale {
            JunkAssessment::new(
                Some(JunkKind::StaleUnused),
                needs_review,
                "app data untouched > 6 months",
            )
        } else {
            JunkAssessment::new(None, needs_review, "")
        };
        return Some((StorageCategory::Apps, assessment));
    }

    if lower_path.contains("/pictures/")
        || lower_path.ends_with("/pictures")
        || lower_path.contains("\\pictures\\")
    {
        return Some((
            StorageCategory::Photos,
            stale_if(is_stale, true, "photo untouched > 6 months"),
        ));
    }

    if lower_path.contains("/movies/")
        || lower_path.ends_with("/movies")
        || lower_path.contains("/videos/")
        || lower_path.ends_with("/videos")
    {
        return Some((
            StorageCategory::Movies,
            stale_if(is_stale, true, "video untouched > 6 months"),
        ));
    }

    if lower_path.contains("/music/")
        || lower_path.ends_with("/music")
        || lower_path.contains("/audio/")
    {
        return Some((
            StorageCategory::Music,
            stale_if(is_stale, true, "audio untouched > 6 months"),
        ));
    }

    if lower_path.contains("/downloads/") && matches!(ext, "dmg" | "pkg" | "iso" | "msi" | "exe") {
        return Some((
            StorageCategory::Documents,
            JunkAssessment::new(
                Some(JunkKind::InstallerPackage),
                false,
                "installer package in Downloads",
            ),
        ));
    }

    None
}

fn classify_by_extension(ext: &str, is_stale: bool) -> Option<(StorageCategory, JunkAssessment)> {
    match ext {
        "app" | "ipa" | "apk" | "appimage" => Some((
            StorageCategory::Apps,
            stale_if(is_stale, true, "application bundle untouched > 6 months"),
        )),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "cc" | "cpp" | "h" | "hpp"
        | "swift" | "kt" | "java" | "nix" | "toml" | "yaml" | "yml" | "lock" | "wasm" | "o"
        | "rlib" | "rmeta" | "a" | "dylib" | "so" | "pdb" | "dsym" => Some((
            StorageCategory::Developer,
            stale_if(is_stale, false, "source or build artifact"),
        )),
        "png" | "jpg" | "jpeg" | "heic" | "heif" | "webp" | "gif" | "raw" | "dng" | "cr2"
        | "nef" | "arw" | "tiff" | "tif" | "svg" | "psd" => Some((
            StorageCategory::Photos,
            stale_if(is_stale, true, "image untouched > 6 months"),
        )),
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "prores" => Some((
            StorageCategory::Movies,
            stale_if(is_stale, true, "video untouched > 6 months"),
        )),
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "aiff" | "opus" => Some((
            StorageCategory::Music,
            stale_if(is_stale, true, "audio untouched > 6 months"),
        )),
        "dmg" | "pkg" | "iso" | "msi" => Some((
            StorageCategory::Documents,
            JunkAssessment::new(
                Some(JunkKind::InstallerPackage),
                true,
                "disk image or installer package",
            ),
        )),
        "log" | "tmp" | "bak" | "swp" => Some((
            StorageCategory::Caches,
            JunkAssessment::new(Some(JunkKind::Cache), false, "temporary or log file"),
        )),
        "plist" | "db" | "sqlite" | "sqlite3" | "kext" | "sys" => Some((
            StorageCategory::SystemData,
            stale_if(is_stale, true, "system/database state"),
        )),
        _ => None,
    }
}

fn is_build_artifact_name(file_name: &str) -> bool {
    matches!(
        file_name,
        "node_modules" | "target" | "deriveddata" | "__pycache__" | ".pytest_cache" | ".next"
    )
}

fn is_developer_path(lower_path: &str) -> bool {
    const DEV_SEGMENTS: [&str; 12] = [
        "/library/developer/",
        "/.cargo/",
        "/.rustup/",
        "/.nix/",
        "/nix/store/",
        "/.git/",
        "/node_modules/",
        "/deriveddata/",
        "/.venv/",
        "/projects/",
        "/developer/",
        "/.local/share/cargo/",
    ];
    DEV_SEGMENTS
        .iter()
        .any(|segment| lower_path.contains(segment))
}

fn is_apps_path(lower_path: &str, ext: &str, kind: EntryKind) -> bool {
    if kind == EntryKind::Directory && ext == "app" {
        return true;
    }
    lower_path.starts_with("/applications")
        || lower_path.contains("/applications/")
        || lower_path.contains("/library/application support/")
        || lower_path.contains("/library/containers/")
        || lower_path.contains("\\program files")
        || lower_path.contains("/usr/share/applications")
}

fn is_system_path(lower_path: &str) -> bool {
    lower_path.starts_with("/system")
        || lower_path.starts_with("/library")
        || lower_path.contains("/library/")
        || lower_path.starts_with("/usr")
        || lower_path.starts_with("/private")
        || lower_path.starts_with("/var")
        || lower_path.starts_with("/etc")
        || lower_path.contains("\\windows\\")
}

const fn stale_if(is_stale: bool, review_first: bool, reason: &'static str) -> JunkAssessment {
    if is_stale {
        JunkAssessment::new(Some(JunkKind::StaleUnused), review_first, reason)
    } else {
        JunkAssessment::CLEAN
    }
}
