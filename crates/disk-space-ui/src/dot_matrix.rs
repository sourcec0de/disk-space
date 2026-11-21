//! Interactive multi-row dot-matrix disk usage visualization matching the screenshot.

use std::f32::consts::TAU;

use disk_space_core::StorageTree;
use disk_space_core::format_bytes;
use disk_space_platform::StorageCategory;
use nsh_core::Bounds;
use nsh_core::Points;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::Interactive;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::MouseEvent;
use nsh_core::prelude::Painter;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::PressEvent;
use nsh_core::prelude::Styled;
use nsh_core::prelude::canvas;
use nsh_core::prelude::computed;
use nsh_core::prelude::px;
use nsh_core::prelude::signal;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::palette;
use crate::state;

const MATRIX_ROWS: usize = 6;
const MATRIX_ROWS_F32: f32 = 6.0;
const DOT_RADIUS: f32 = 2.1;
const DOT_PITCH: f32 = 7.0;
const CANVAS_HEIGHT: f32 = 50.0;

#[derive(Debug, Clone)]
struct MatrixColumn {
    category: Option<StorageCategory>,
    is_staged_tail: bool,
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "canvas pixel geometry converts layout points to discrete dot grid columns"
)]
fn build_columns(tree: &StorageTree, total_cols: usize, staged_bytes: u64) -> Vec<MatrixColumn> {
    if total_cols == 0 {
        return Vec::new();
    }

    let root_node = tree.node(tree.root_id());
    let root_cat_totals = root_node
        .map(|node| node.category_totals)
        .unwrap_or_default();
    let scanned_total = root_cat_totals.total();

    let vol = tree.volume();
    let denom = if vol.total_bytes >= scanned_total && vol.total_bytes > 0 {
        vol.total_bytes
    } else {
        scanned_total.max(1)
    };

    let mut used_cols =
        ((scanned_total as f64 / denom as f64) * total_cols as f64).round() as usize;
    if scanned_total > 0 && used_cols == 0 {
        used_cols = 1;
    }
    used_cols = used_cols.min(total_cols);

    let staged_cols = if staged_bytes > 0 && used_cols > 0 {
        ((staged_bytes as f64 / scanned_total.max(1) as f64) * used_cols as f64).round() as usize
    } else {
        0
    }
    .min(used_cols);

    let non_empty: Vec<(StorageCategory, u64)> = StorageCategory::ALL
        .iter()
        .copied()
        .filter_map(|cat| {
            let bytes = root_cat_totals.get(cat);
            (bytes > 0).then_some((cat, bytes))
        })
        .collect();

    if non_empty.is_empty() {
        return vec![
            MatrixColumn {
                category: None,
                is_staged_tail: false,
            };
            total_cols
        ];
    }

    let cols_per_cat = allocate_category_columns(&non_empty, scanned_total, used_cols);
    let mut columns = Vec::with_capacity(total_cols);
    for (cat, count) in cols_per_cat {
        for _ in 0..count {
            if columns.len() < total_cols {
                columns.push(MatrixColumn {
                    category: Some(cat),
                    is_staged_tail: false,
                });
            }
        }
    }

    if staged_cols > 0 {
        let start = columns.len().saturating_sub(staged_cols);
        for col in &mut columns[start..] {
            col.is_staged_tail = true;
        }
    }

    while columns.len() < total_cols {
        columns.push(MatrixColumn {
            category: None,
            is_staged_tail: false,
        });
    }

    columns
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "proportional column rounding maps u64 byte ratios to integer grid counts"
)]
fn allocate_category_columns(
    non_empty: &[(StorageCategory, u64)],
    scanned_total: u64,
    used_cols: usize,
) -> Vec<(StorageCategory, usize)> {
    let mut cols_per_cat: Vec<(StorageCategory, usize)> = Vec::with_capacity(non_empty.len());
    let mut allocated = 0_usize;

    for &(cat, bytes) in non_empty {
        let share =
            ((bytes as f64 / scanned_total.max(1) as f64) * used_cols as f64).floor() as usize;
        let clamped = share.max(1);
        cols_per_cat.push((cat, clamped));
        allocated = allocated.saturating_add(clamped);
    }

    while allocated > used_cols {
        if let Some(max_entry) = cols_per_cat.iter_mut().max_by_key(|(_, count)| *count)
            && max_entry.1 > 1
        {
            max_entry.1 -= 1;
            allocated -= 1;
        } else {
            break;
        }
    }
    while allocated < used_cols {
        if let Some((_, best_cat)) = non_empty
            .iter()
            .enumerate()
            .max_by_key(|(_, (_, bytes))| *bytes)
            .map(|(idx, (cat, _))| (idx, *cat))
        {
            if let Some(entry) = cols_per_cat.iter_mut().find(|(c, _)| *c == best_cat) {
                entry.1 += 1;
                allocated += 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    cols_per_cat
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "maps pointer X coordinate to matrix column index"
)]
fn column_at_x(width: f32, x: f32, col_count: usize) -> Option<usize> {
    if width <= 0.0 || col_count == 0 {
        return None;
    }
    let idx = ((x / DOT_PITCH).floor() as isize).max(0) as usize;
    (idx < col_count).then_some(idx)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "dot grid converts layout point coordinates to discrete columns and rows"
)]
fn render_dot_matrix(
    w: f32,
    selected_category: Option<StorageCategory>,
    hover_category: Option<StorageCategory>,
    staged_bytes: u64,
) -> Painter {
    if w <= 10.0 {
        return Painter::new();
    }
    let Some(tree) = state::current_tree() else {
        return draw_idle_dots(w);
    };

    let total_cols = ((w - 4.0) / DOT_PITCH).floor().max(1.0) as usize;
    let columns = build_columns(&tree, total_cols, staged_bytes);
    let oy = (MATRIX_ROWS_F32 - 1.0).mul_add(-DOT_PITCH, CANVAS_HEIGHT) / 2.0;

    let mut painter = Painter::new();
    for (col_idx, col) in columns.iter().enumerate() {
        let cx = (col_idx as f32).mul_add(DOT_PITCH, 4.0);
        let is_dimmed = match (selected_category, hover_category, col.category) {
            (Some(sel), _, Some(cat)) => sel != cat,
            (None, Some(hov), Some(cat)) => hov != cat,
            _ => false,
        };
        let dot_color = col.category.map_or(palette::DOT_EMPTY, |cat| {
            let alpha = if is_dimmed { 0.28 } else { 0.92 };
            palette::category_color(cat).opacity(alpha)
        });

        for row in 0..MATRIX_ROWS {
            let cy = (row as f32).mul_add(DOT_PITCH, oy);
            let fill = if col.is_staged_tail && row == 0 {
                palette::DANGER_RED
            } else {
                dot_color
            };
            painter = painter
                .fill_color(fill)
                .begin_path()
                .arc(cx, cy, DOT_RADIUS, 0.0, TAU)
                .fill();
        }
    }
    painter
}

/// Interactive dot-matrix disk map section.
pub(crate) fn dot_matrix_bar() -> impl IntoElement {
    let width = signal(0.0_f32);
    let selected_cat = computed(|| state::browse_signal().with(|b| b.selected_category));
    let staged_bytes = computed(|| state::snag_signal().with(|s| s.queue.total_bytes()));

    let matrix_canvas = canvas(move || {
        let hover_cat = state::hover_signal().with(|h| h.category);
        render_dot_matrix(
            width.get(),
            selected_cat.get(),
            hover_cat,
            staged_bytes.get(),
        )
    })
    .h(px(CANVAS_HEIGHT))
    .w_full()
    .cursor(Cursor::Pointer)
    .on_layout(move |frame: &Bounds<Points>| width.set(frame.size.width.0))
    .on_mouse_move(move |event: &MouseEvent| {
        let Some(tree) = state::current_tree_untracked() else {
            return;
        };
        let w = width.get_untracked();
        let staged = state::snag_signal().with_untracked(|snag| snag.queue.total_bytes());
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "width converted to column count"
        )]
        let total_cols = ((w - 4.0) / DOT_PITCH).floor().max(1.0) as usize;
        let columns = build_columns(&tree, total_cols, staged);
        if let Some(col_idx) = column_at_x(w, event.position.x.0, columns.len()) {
            let hovered = columns[col_idx].category;
            state::hover_signal().update(|hover| hover.category = hovered);
        }
    })
    .on_mouse_leave(move |_: &MouseEvent| {
        state::hover_signal().update(|hover| hover.category = None);
    })
    .on_press(move |event: &PressEvent| {
        let Some(tree) = state::current_tree_untracked() else {
            return;
        };
        let w = width.get_untracked();
        let staged = state::snag_signal().with_untracked(|snag| snag.queue.total_bytes());
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "width converted to column count"
        )]
        let total_cols = ((w - 4.0) / DOT_PITCH).floor().max(1.0) as usize;
        let columns = build_columns(&tree, total_cols, staged);
        if let Some(col_idx) = column_at_x(w, event.position.x.0, columns.len())
            && let Some(cat) = columns[col_idx].category
        {
            let currently_selected =
                state::browse_signal().with_untracked(|browse| browse.selected_category);
            if currently_selected == Some(cat) {
                state::select_category(None);
            } else {
                state::select_category(Some(cat));
            }
        }
    });

    let hover_caption = text(move || {
        let Some(tree) = state::current_tree() else {
            return String::new();
        };
        let hover_cat = state::hover_signal().with(|h| h.category);
        let active = hover_cat.or_else(|| selected_cat.get());
        active.map_or_else(
            || "click any category segment to isolate it".to_owned(),
            |cat| {
                let bytes = tree
                    .node(tree.root_id())
                    .map_or(0, |node| node.category_totals.get(cat));
                format!(
                    "{} · {} (click dot segment to toggle filter)",
                    cat.label(),
                    format_bytes(bytes)
                )
            },
        )
    })
    .font_family(palette::mono_font())
    .text_size(px(11.0))
    .text_color(palette::TEXT_FAINT);

    view()
        .w_full()
        .gap_1()
        .child(matrix_canvas)
        .child(hover_caption)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "idle placeholder grid converts width points to discrete columns"
)]
fn draw_idle_dots(width: f32) -> Painter {
    let total_cols = ((width - 4.0) / DOT_PITCH).floor().max(1.0) as usize;
    let oy = (MATRIX_ROWS_F32 - 1.0).mul_add(-DOT_PITCH, CANVAS_HEIGHT) / 2.0;
    let mut painter = Painter::new().fill_color(palette::DOT_EMPTY);
    for col in 0..total_cols {
        let cx = (col as f32).mul_add(DOT_PITCH, 4.0);
        for row in 0..MATRIX_ROWS {
            let cy = (row as f32).mul_add(DOT_PITCH, oy);
            painter = painter
                .begin_path()
                .arc(cx, cy, DOT_RADIUS, 0.0, TAU)
                .fill();
        }
    }
    painter
}
