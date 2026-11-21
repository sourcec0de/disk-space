//! Human-readable byte size, age, and path formatting helpers.

use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;

/// Seconds per day (24h).
const SECS_PER_DAY: u64 = 86_400;
/// Threshold for flagging an entry as stale/unused (180 days).
const STALE_DAYS: u64 = 180;

/// Freshness bucket for a filesystem timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AgeFreshness {
    /// Touched within the last 24 hours.
    Today,
    /// Touched between 1 day and 180 days ago.
    Recent,
    /// Untouched for 180 days or more.
    Stale,
    /// Timestamp unavailable.
    Unknown,
}

/// Formatted relative age paired with its freshness classification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FormattedAge {
    /// Compact label such as `"today"`, `"3d"`, `"5mo"`, `"1y"`, or `"--"`.
    pub label: String,
    /// Freshness classification for color styling.
    pub freshness: AgeFreshness,
}

impl FormattedAge {
    /// Constructs a [`FormattedAge`].
    #[must_use]
    pub const fn new(label: String, freshness: AgeFreshness) -> Self {
        Self { label, freshness }
    }
}

/// Formats `bytes` using SI decimal units (`GB`, `MB`, `KB`, `B`), matching macOS Finder.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "display formatting of byte counts accepts f64 precision above 2^53"
)]
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;

    let val = bytes as f64;
    if val >= TB {
        format!("{:.1} TB", val / TB)
    } else if val >= GB {
        format!("{:.1} GB", val / GB)
    } else if val >= MB {
        if val >= 100.0 * MB {
            format!("{:.0} MB", val / MB)
        } else {
            format!("{:.1} MB", val / MB)
        }
    } else if val >= KB {
        format!("{:.0} KB", val / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Formats a timestamp relative to `now` into compact labels (`today`, `5mo`, `1y`).
#[must_use]
pub fn format_age(timestamp: Option<SystemTime>, now: SystemTime) -> FormattedAge {
    let Some(ts) = timestamp else {
        return FormattedAge::new("--".to_owned(), AgeFreshness::Unknown);
    };
    let age = now.duration_since(ts).unwrap_or(Duration::ZERO);
    let days = age.as_secs() / SECS_PER_DAY;

    if days == 0 {
        FormattedAge::new("today".to_owned(), AgeFreshness::Today)
    } else if days < 30 {
        FormattedAge::new(format!("{days}d"), AgeFreshness::Recent)
    } else if days < 365 {
        let months = (days / 30).max(1);
        let freshness = if days >= STALE_DAYS {
            AgeFreshness::Stale
        } else {
            AgeFreshness::Recent
        };
        FormattedAge::new(format!("{months}mo"), freshness)
    } else {
        let years = (days / 365).max(1);
        FormattedAge::new(format!("{years}y"), AgeFreshness::Stale)
    }
}

/// Formats `path` for display, abbreviating `home` prefix with `~` when present.
#[must_use]
pub fn format_display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home_dir) = home
        && let Ok(rel) = path.strip_prefix(home_dir)
    {
        if rel.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_si_byte_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(4_000), "4 KB");
        assert_eq!(format_bytes(832_000_000), "832 MB");
        assert_eq!(format_bytes(5_400_000_000), "5.4 GB");
        assert_eq!(format_bytes(401_300_000_000), "401.3 GB");
    }

    #[test]
    fn formats_ages_and_stale_freshness() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let recent = now - Duration::from_secs(2 * SECS_PER_DAY);
        let stale = now - Duration::from_secs(400 * SECS_PER_DAY);

        assert_eq!(format_age(Some(now), now).label, "today");
        assert_eq!(
            format_age(Some(recent), now).freshness,
            AgeFreshness::Recent
        );
        assert_eq!(format_age(Some(stale), now).label, "1y");
        assert_eq!(format_age(Some(stale), now).freshness, AgeFreshness::Stale);
    }
}
