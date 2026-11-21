//! Interactive DaisyDisk-inspired concentric radial sunburst chart on NSH Canvas.

use std::f32::consts::FRAC_PI_2;
use std::f32::consts::TAU;

use disk_space_core::NodeId;
use disk_space_core::StorageTree;
use disk_space_core::format_bytes;
use disk_space_platform::EntryKind;
use nsh_core::Bounds;
use nsh_core::Points;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::Interactive;
use nsh_core::prelude::MouseEvent;
use nsh_core::prelude::Painter;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::PressEvent;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::canvas;
use nsh_core::prelude::px;
use nsh_core::prelude::signal;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::palette;
use crate::state;

const SUNBURST_SIZE: f32 = 340.0;
const HUB_RADIUS: f32 = 44.0;
const RING_WIDTH: f32 = 32.0;
const MAX_RINGS: usize = 4;

#[derive(Debug, Clone)]
struct SunburstSlice {
    node_id: NodeId,
    ring: usize,
    start_angle: f32,
    end_angle: f32,
    r_inner: f32,
    r_outer: f32,
}

fn collect_slices(tree: &StorageTree, center_node: NodeId) -> Vec<SunburstSlice> {
    let mut slices = Vec::new();
    let Some(root_node) = tree.node(center_node) else {
        return slices;
    };
    if root_node.total_bytes == 0 {
        return slices;
    }

    collect_ring(
        tree,
        center_node,
        0,
        -FRAC_PI_2,
        -FRAC_PI_2 + TAU,
        &mut slices,
    );
    slices
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "angular layout converts u64 byte ratios to f32 angles"
)]
fn collect_ring(
    tree: &StorageTree,
    parent_id: NodeId,
    ring: usize,
    start_angle: f32,
    end_angle: f32,
    out: &mut Vec<SunburstSlice>,
) {
    if ring >= MAX_RINGS {
        return;
    }
    let Some(parent) = tree.node(parent_id) else {
        return;
    };
    let span = end_angle - start_angle;
    if span < 0.035 || parent.total_bytes == 0 {
        return;
    }

    let r_inner = (ring as f32).mul_add(RING_WIDTH, HUB_RADIUS);
    let r_outer = r_inner + RING_WIDTH - 2.0;

    let children = tree.children_sorted(parent_id);
    let mut cursor = start_angle;
    for child_id in children {
        let Some(child) = tree.node(child_id) else {
            continue;
        };
        if child.total_bytes == 0 {
            continue;
        }
        let wedge = span * (child.total_bytes as f64 / parent.total_bytes as f64) as f32;
        if wedge < 0.03 {
            continue;
        }
        let next_angle = cursor + wedge;
        out.push(SunburstSlice {
            node_id: child_id,
            ring,
            start_angle: cursor,
            end_angle: next_angle,
            r_inner,
            r_outer,
        });
        if child.kind == EntryKind::Directory {
            collect_ring(tree, child_id, ring + 1, cursor, next_angle, out);
        }
        cursor = next_angle;
    }
}

fn hit_test_slice(slices: &[SunburstSlice], cx: f32, cy: f32, px_x: f32, px_y: f32) -> HitResult {
    let dx = px_x - cx;
    let dy = px_y - cy;
    let dist = dx.hypot(dy);
    if dist <= HUB_RADIUS {
        return HitResult::CenterHub;
    }

    let mut angle = dy.atan2(dx);
    if angle < -FRAC_PI_2 {
        angle += TAU;
    }

    for slice in slices {
        if dist >= slice.r_inner
            && dist <= slice.r_outer
            && angle >= slice.start_angle
            && angle <= slice.end_angle
        {
            return HitResult::Slice(slice.node_id);
        }
    }
    HitResult::Outside
}

#[derive(Clone, Copy)]
enum HitResult {
    CenterHub,
    Slice(NodeId),
    Outside,
}

fn render_sunburst(w: f32, h: f32, current_node: Option<NodeId>) -> Painter {
    let Some(active_tree) = state::current_tree() else {
        return Painter::new();
    };
    let hover_sunburst = state::hover_signal().with(|hover| hover.sunburst);
    let center_id = current_node.unwrap_or_else(|| active_tree.root_id());
    let slices = collect_slices(&active_tree, center_id);
    let cx = w / 2.0;
    let cy = h / 2.0;

    let mut painter = Painter::new();
    for slice in &slices {
        let Some(node) = active_tree.node(slice.node_id) else {
            continue;
        };
        let is_hovered = hover_sunburst == Some(slice.node_id);
        let is_snagged =
            state::snag_signal().with(|snag| snag.queue.is_covered(&node.relative_path));

        let base = if is_snagged {
            palette::DANGER_RED
        } else {
            palette::category_color(node.category)
        };
        let ring_opacity = match slice.ring {
            0 => 0.92,
            1 => 0.78,
            2 => 0.64,
            _ => 0.50,
        };
        let fill = if is_hovered {
            base.opacity(1.0)
        } else {
            base.opacity(ring_opacity)
        };

        painter = painter
            .fill_color(fill)
            .stroke_color(palette::BG_CANVAS)
            .line_width(1.25)
            .begin_path()
            .arc(cx, cy, slice.r_outer, slice.start_angle, slice.end_angle)
            .arc_counterclockwise(cx, cy, slice.r_inner, slice.end_angle, slice.start_angle)
            .close_path()
            .fill()
            .stroke();
    }

    painter
        .fill_color(palette::BG_INSET)
        .stroke_color(palette::BORDER_ACTIVE)
        .line_width(1.25)
        .begin_path()
        .arc(cx, cy, HUB_RADIUS - 2.0, 0.0, TAU)
        .fill()
        .stroke()
}

/// Interactive radial sunburst panel for DaisyDisk-style visual drill-down.
pub(crate) fn sunburst_panel(tree: &StorageTree, current_node: Option<NodeId>) -> View {
    let canvas_size = signal((SUNBURST_SIZE, SUNBURST_SIZE));
    let center_node_id = current_node.unwrap_or_else(|| tree.root_id());
    let center_name = tree
        .node(center_node_id)
        .map_or_else(|| "root".to_owned(), |node| node.name.clone());
    let center_bytes = tree.node(center_node_id).map_or(0, |node| node.total_bytes);
    let can_ascend = center_node_id != tree.root_id();

    let chart = canvas(move || {
        let (w, h) = canvas_size.get();
        render_sunburst(w, h, current_node)
    })
    .w(px(SUNBURST_SIZE))
    .h(px(SUNBURST_SIZE))
    .cursor(Cursor::Pointer)
    .on_layout(move |frame: &Bounds<Points>| {
        canvas_size.set((frame.size.width.0, frame.size.height.0));
    })
    .on_mouse_move(move |event: &MouseEvent| {
        let Some(active_tree) = state::current_tree_untracked() else {
            return;
        };
        let center_id = state::browse_signal()
            .with_untracked(|b| b.current_node)
            .unwrap_or_else(|| active_tree.root_id());
        let slices = collect_slices(&active_tree, center_id);
        let (w, h) = canvas_size.get_untracked();
        let hit = hit_test_slice(
            &slices,
            w / 2.0,
            h / 2.0,
            event.position.x.0,
            event.position.y.0,
        );
        let next_hover = match hit {
            HitResult::Slice(id) => Some(id),
            HitResult::CenterHub | HitResult::Outside => None,
        };
        state::hover_signal().update(|hover| hover.sunburst = next_hover);
    })
    .on_mouse_leave(move |_: &MouseEvent| {
        state::hover_signal().update(|hover| hover.sunburst = None);
    })
    .on_press(move |event: &PressEvent| {
        let Some(active_tree) = state::current_tree_untracked() else {
            return;
        };
        let center_id = state::browse_signal()
            .with_untracked(|b| b.current_node)
            .unwrap_or_else(|| active_tree.root_id());
        let slices = collect_slices(&active_tree, center_id);
        let (w, h) = canvas_size.get_untracked();
        match hit_test_slice(
            &slices,
            w / 2.0,
            h / 2.0,
            event.position.x.0,
            event.position.y.0,
        ) {
            HitResult::CenterHub => state::ascend_directory(),
            HitResult::Slice(id) => state::open_node(id),
            HitResult::Outside => {}
        }
    });

    let hovered_info = sunburst_info_card(&center_name, center_bytes, can_ascend);

    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap_6()
        .py_2()
        .child(chart)
        .child(hovered_info)
}

fn sunburst_info_card(center_name: &str, center_bytes: u64, can_ascend: bool) -> View {
    view().flex_1().gap_2().justify_center().child(
        view()
            .p_4()
            .rounded_lg()
            .bg(palette::BG_SURFACE)
            .border_1()
            .border_color(palette::BORDER_SUBTLE)
            .gap_2()
            .child(
                text(format!(
                    "center: {center_name} · {}{}",
                    format_bytes(center_bytes),
                    if can_ascend {
                        " (click hub to go up)"
                    } else {
                        ""
                    }
                ))
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_PRIMARY),
            )
            .child(
                text(move || {
                    let Some(active_tree) = state::current_tree() else {
                        return String::new();
                    };
                    let hov = state::hover_signal().with(|h| h.sunburst);
                    hov.and_then(|id| active_tree.node(id)).map_or_else(
                        || "hover a ring arc to inspect · click arc to drill down".to_owned(),
                        |node| {
                            format!(
                                "{} · {} · {} item(s)",
                                node.name,
                                format_bytes(node.total_bytes),
                                node.file_count + node.dir_count
                            )
                        },
                    )
                })
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_MUTED),
            ),
    )
}
