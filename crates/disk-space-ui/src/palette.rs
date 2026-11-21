//! Obsidian color palette, typography helpers, and shared miniature dot-bar visuals.

use disk_space_platform::StorageCategory;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Rgba;
use nsh_core::prelude::Styled;
use nsh_core::prelude::px;
use nsh_core::prelude::rgb;
use nsh_core::prelude::view;

/// Primary obsidian window canvas (`#08080a`).
pub(crate) const BG_CANVAS: Rgba = rgb(0x0008_080a);
/// Subtle elevated card background (`#0f1014`).
pub(crate) const BG_SURFACE: Rgba = rgb(0x000f_1014);
/// Inset container / input / drawer background (`#14151a`).
pub(crate) const BG_INSET: Rgba = rgb(0x0014_151a);
/// Active / selected row highlight (`#131722`).
pub(crate) const BG_SELECTED: Rgba = rgb(0x0013_1722);
/// Hover row subtle highlight (`#101219`).
pub(crate) const BG_HOVER: Rgba = rgb(0x0010_1219);
/// Hairline border divider (`#1e2028`).
pub(crate) const BORDER_SUBTLE: Rgba = rgb(0x001e_2028);
/// Stronger active border (`#2c303d`).
pub(crate) const BORDER_ACTIVE: Rgba = rgb(0x002c_303d);

/// Primary crisp off-white text (`#eceef2`).
pub(crate) const TEXT_PRIMARY: Rgba = rgb(0x00ec_eef2);
/// Secondary muted gray text (`#7c818d`).
pub(crate) const TEXT_MUTED: Rgba = rgb(0x007c_818d);
/// Faint dim label text (`#454954`).
pub(crate) const TEXT_FAINT: Rgba = rgb(0x0045_4954);

/// Faint unallocated dot in dot-matrix & mini meters (`#1b1d24`).
pub(crate) const DOT_EMPTY: Rgba = rgb(0x001b_1d24);

/// Amber warning / stale / "unused" highlight (`#eab308`).
pub(crate) const WARN_AMBER: Rgba = rgb(0x00ea_b308);
/// Soft translucent amber background (`#221a09`).
pub(crate) const WARN_SURFACE: Rgba = rgb(0x0022_1a09);

/// Destructive red accent (`#ef4444`).
pub(crate) const DANGER_RED: Rgba = rgb(0x00ef_4444);
/// Soft destructive surface (`#261114`).
pub(crate) const DANGER_SURFACE: Rgba = rgb(0x0026_1114);

/// Safety-jail emerald accent (`#22c55e`).
pub(crate) const SAFE_GREEN: Rgba = rgb(0x0022_c55e);

/// Returns the fixed-width monospace font family appropriate for the target OS.
#[must_use]
pub(crate) const fn mono_font() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(target_os = "windows")]
    {
        "Consolas"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "monospace"
    }
}

/// Returns the screenshot-matched accent color for `category`.
#[must_use]
pub(crate) const fn category_color(category: StorageCategory) -> Rgba {
    match category {
        StorageCategory::Apps => rgb(0x003b_82f6),
        StorageCategory::Developer => rgb(0x0022_c55e),
        StorageCategory::Photos => rgb(0x00ec_4899),
        StorageCategory::Movies => rgb(0x00a8_55f7),
        StorageCategory::Documents => rgb(0x0038_bdf8),
        StorageCategory::Music => rgb(0x00f5_9e0b),
        StorageCategory::SystemData => rgb(0x00a1_a1aa),
        StorageCategory::Caches => rgb(0x00f9_7316),
        StorageCategory::Trash => rgb(0x00ef_4444),
        _ => TEXT_MUTED,
    }
}

/// Renders a horizontal row of proportional miniature dots matching the screenshot meters.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "dot meter maps clamped [0.0, 1.0] fraction to small integer dot count"
)]
pub(crate) fn mini_dot_bar(share: f32, active: Rgba, total_dots: usize) -> impl IntoElement {
    let clamped = share.clamp(0.0, 1.0);
    let filled = if clamped <= 0.005 {
        0
    } else {
        ((clamped * total_dots as f32).round() as usize).clamp(1, total_dots)
    };

    view()
        .flex_row()
        .items_center()
        .gap(px(2.5))
        .children((0..total_dots).map(move |idx| {
            let color = if idx < filled { active } else { DOT_EMPTY };
            view().w(px(3.0)).h(px(3.0)).rounded_full().bg(color)
        }))
}
