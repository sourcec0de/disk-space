//! Main explorer pane: breadcrumbs, search & junk filters, item rows, and aggregates.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use disk_space_core::AgeFreshness;
use disk_space_core::DiskNode;
use disk_space_core::FilterMode;
use disk_space_core::NodeId;
use disk_space_core::SortOrder;
use disk_space_core::StorageTree;
use disk_space_core::compute_aggregates;
use disk_space_core::format_age;
use disk_space_core::format_bytes;
use disk_space_platform::EntryKind;
use disk_space_platform::StorageCategory;
use nsh_base::button;
use nsh_base::input;
use nsh_core::prelude::Accessible;
use nsh_core::prelude::ComponentBuilder;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::Easing;
use nsh_core::prelude::Element;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::Keyframe;
use nsh_core::prelude::KeyframeTiming;
use nsh_core::prelude::MotionChannel;
use nsh_core::prelude::MotionSpec;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::ReadSignal;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::computed;
use nsh_core::prelude::dynamic;
use nsh_core::prelude::each;
use nsh_core::prelude::effect;
use nsh_core::prelude::motion;
use nsh_core::prelude::px;
use nsh_core::prelude::scroll;
use nsh_core::prelude::text;
use nsh_core::prelude::view;
use nsh_core::prelude::when;

use crate::palette;
use crate::state;
use crate::state::AggregateKind;
use crate::state::ViewTab;
use crate::sunburst;

/// Renders the right detail pane (breadcrumbs, search/filter strip, rows or aggregates).
pub(crate) fn explorer_pane(tree: &Arc<StorageTree>) -> impl IntoElement {
    let header_tree = Arc::clone(tree);
    let filter_tree = Arc::clone(tree);
    let tab_tree = Arc::clone(tree);

    let active_tab = computed(move || state::browse_signal().with(|b| b.view_tab));

    let main_body = dynamic(move || {
        let tab = active_tab.get();
        match tab {
            ViewTab::Sunburst => {
                let sun_tree = Arc::clone(&tab_tree);
                view()
                    .flex_1()
                    .gap_3()
                    .child(dynamic(move || sunburst::sunburst_panel(&sun_tree)))
                    .child(item_list_section(&tab_tree))
            }
            ViewTab::Aggregates => aggregates_section(Arc::clone(&tab_tree)),
            ViewTab::DotMatrix => item_list_section(&tab_tree),
        }
    });

    view()
        .flex_1()
        .gap_3()
        .child(dynamic(move || build_top_header(&header_tree)))
        .child(build_search_bar())
        .child(dynamic(move || build_filter_strip(&filter_tree)))
        .child(main_body)
}

fn build_top_header(tree: &StorageTree) -> View {
    let browse = state::browse_signal().get();
    let header_node_id = browse.current_node.unwrap_or_else(|| tree.root_id());
    let header_node = tree.node(header_node_id);

    let (header_bytes, review_first) = match (browse.current_node, browse.selected_category) {
        (None, Some(cat)) => {
            let root_node = tree.node(tree.root_id());
            (root_node.map_or(0, |n| n.category_totals.get(cat)), false)
        }
        _ => (
            header_node.map_or(0, |n| n.total_bytes),
            header_node.is_some_and(|n| n.review_first),
        ),
    };

    let vol_total = tree.volume().total_bytes.max(1);
    let pct_of_disk = header_bytes.saturating_mul(100) / vol_total;
    let active_accent = browse
        .selected_category
        .or_else(|| header_node.map(|n| n.category))
        .map_or(palette::TEXT_PRIMARY, palette::category_color);

    let subtitle_path = match (browse.current_node, browse.selected_category) {
        (None, Some(cat)) => format!(
            "{} · {} category view",
            state::display_abs_path(tree.root().canonical_path()),
            cat.label()
        ),
        (Some(id), _) => state::display_abs_path(&tree.absolute_path(id)),
        (None, None) => state::display_abs_path(tree.root().canonical_path()),
    };

    let breadcrumbs = build_breadcrumbs(
        tree,
        browse.selected_category,
        browse.current_node,
        active_accent,
    );

    view()
        .gap_1()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_3()
                .child(breadcrumbs)
                .child(header_metrics_group(
                    header_bytes,
                    pct_of_disk,
                    review_first,
                )),
        )
        .child(
            text(subtitle_path)
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_FAINT)
                .truncate(),
        )
}

fn header_metrics_group(header_bytes: u64, pct_of_disk: u64, review_first: bool) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            text(format_bytes(header_bytes))
                .font_family(palette::mono_font())
                .font_weight(FontWeight::BOLD)
                .tabular_nums()
                .text_size(px(14.0))
                .text_color(palette::TEXT_PRIMARY),
        )
        .child(
            text(format!("{pct_of_disk}% of disk"))
                .font_family(palette::mono_font())
                .tabular_nums()
                .text_size(px(12.0))
                .text_color(palette::TEXT_MUTED),
        )
        .child(if review_first {
            view()
                .px(px(6.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(palette::WARN_SURFACE)
                .child(
                    text("review first")
                        .font_family(palette::mono_font())
                        .text_size(px(11.0))
                        .text_color(palette::WARN_AMBER),
                )
        } else {
            view()
        })
}

fn build_search_bar() -> View {
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            input()
                .label("Search storage")
                .value(move || state::browse_signal().with(|b| b.search_text.clone()))
                .on_change(move |value: &str| {
                    state::browse_signal().update(|b| {
                        value.clone_into(&mut b.search_text);
                        b.selected_row = 0;
                    });
                })
                .placeholder("/ search name, subpath, or .ext...")
                .flex_1()
                .px_3()
                .py(px(6.0))
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .bg(palette::BG_INSET)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .rounded_md()
                .text_color(palette::TEXT_PRIMARY)
                .placeholder_color(palette::TEXT_FAINT),
        )
        .child(when(
            move || state::browse_signal().with(|b| !b.search_text.is_empty()),
            clear_search_btn,
        ))
}

fn clear_search_btn() -> nsh_base::ButtonElement {
    button()
        .accessibility_label("Clear search")
        .on_press(|| {
            state::browse_signal().update(|b| {
                b.search_text.clear();
                b.selected_row = 0;
            });
        })
        .px_2()
        .py(px(6.0))
        .rounded_md()
        .bg(palette::BG_INSET)
        .border_1()
        .border_color(palette::BORDER_SUBTLE)
        .cursor(Cursor::Pointer)
        .child(
            text("clear")
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(palette::TEXT_MUTED),
        )
}

fn build_filter_strip(tree: &StorageTree) -> View {
    let browse = state::browse_signal().get();
    let header_node_id = browse.current_node.unwrap_or_else(|| tree.root_id());
    let unused_bytes = match (browse.current_node, browse.selected_category) {
        (None, Some(cat)) => tree
            .category_entries(cat)
            .iter()
            .filter_map(|id| tree.node(*id))
            .map(|n| n.unused_bytes)
            .sum(),
        _ => tree.node(header_node_id).map_or(0, |n| n.unused_bytes),
    };

    let junk_only = browse.filter_mode == FilterMode::JunkAndUnusedOnly;
    view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            text(if unused_bytes > 0 {
                format!("{} probably unused", format_bytes(unused_bytes))
            } else {
                "no unused junk flagged in scope".to_owned()
            })
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(12.0))
            .text_color(if unused_bytes > 0 {
                palette::WARN_AMBER
            } else {
                palette::TEXT_FAINT
            }),
        )
        .child(filter_controls(junk_only, browse.sort_order))
}

fn filter_controls(junk_only: bool, sort_order: SortOrder) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            button()
                .accessibility_label("Toggle junk and unused filter")
                .on_press(|| {
                    state::browse_signal().update(|b| {
                        b.filter_mode = match b.filter_mode {
                            FilterMode::All => FilterMode::JunkAndUnusedOnly,
                            FilterMode::JunkAndUnusedOnly | _ => FilterMode::All,
                        };
                        b.selected_row = 0;
                    });
                })
                .px_2()
                .py(px(3.0))
                .rounded_sm()
                .cursor(Cursor::Pointer)
                .bg(if junk_only {
                    palette::WARN_SURFACE
                } else {
                    palette::BG_CANVAS
                })
                .border_1()
                .border_color(if junk_only {
                    palette::WARN_AMBER
                } else {
                    palette::BORDER_SUBTLE
                })
                .child(
                    text(if junk_only {
                        "f showing junk & unused"
                    } else {
                        "f show only junk & unused"
                    })
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(if junk_only {
                        palette::WARN_AMBER
                    } else {
                        palette::TEXT_MUTED
                    }),
                ),
        )
        .child(
            button()
                .accessibility_label("Cycle sort order")
                .on_press(|| {
                    state::browse_signal().update(|b| {
                        b.sort_order = match b.sort_order {
                            SortOrder::SizeDesc => SortOrder::AgeOldestFirst,
                            SortOrder::AgeOldestFirst => SortOrder::NameAsc,
                            SortOrder::NameAsc | _ => SortOrder::SizeDesc,
                        };
                        b.selected_row = 0;
                    });
                })
                .px_2()
                .py(px(3.0))
                .rounded_sm()
                .cursor(Cursor::Pointer)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .child(
                    text(match sort_order {
                        SortOrder::AgeOldestFirst => "sort: age",
                        SortOrder::NameAsc => "sort: name",
                        SortOrder::SizeDesc | _ => "sort: size",
                    })
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::TEXT_MUTED),
                ),
        )
}

fn build_breadcrumbs(
    tree: &StorageTree,
    selected_category: Option<StorageCategory>,
    current_node: Option<NodeId>,
    accent: nsh_core::prelude::Rgba,
) -> impl IntoElement {
    let mut crumbs: Vec<(String, Option<NodeId>, bool)> = Vec::new();
    match (selected_category, current_node) {
        (Some(cat), None) => {
            crumbs.push((cat.label().to_owned(), None, true));
        }
        (Some(cat), Some(curr)) => {
            crumbs.push((cat.label().to_owned(), None, false));
            let chain = tree.ancestors(curr);
            for (idx, id) in chain.iter().copied().enumerate() {
                if id == tree.root_id() {
                    continue;
                }
                if let Some(node) = tree.node(id) {
                    let is_last = idx + 1 == chain.len();
                    crumbs.push((format!("/{}", node.name), Some(id), is_last));
                }
            }
        }
        (None, Some(curr)) => {
            let chain = tree.ancestors(curr);
            for (idx, id) in chain.iter().copied().enumerate() {
                if let Some(node) = tree.node(id) {
                    let label = if idx == 0 {
                        node.name.clone()
                    } else {
                        format!("/{}", node.name)
                    };
                    let is_last = idx + 1 == chain.len();
                    crumbs.push((label, Some(id), is_last));
                }
            }
        }
        (None, None) => {
            let root_name = tree
                .node(tree.root_id())
                .map_or_else(|| "root".to_owned(), |n| n.name.clone());
            crumbs.push((root_name, None, true));
        }
    }

    view()
        .flex_row()
        .items_center()
        .flex_wrap()
        .gap_1()
        .children(crumbs.into_iter().map(move |(label, target, is_last)| {
            button()
                .accessibility_label(format!("Breadcrumb {label}"))
                .on_press(move || {
                    state::browse_signal().update(|b| {
                        b.current_node = target;
                        b.selected_row = 0;
                    });
                })
                .cursor(Cursor::Pointer)
                .child(
                    text(label)
                        .font_family(palette::mono_font())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(14.0))
                        .text_color(if is_last {
                            accent
                        } else {
                            palette::TEXT_PRIMARY
                        }),
                )
        }))
}

fn item_list_section(tree: &Arc<StorageTree>) -> View {
    let count_tree = Arc::clone(tree);
    let items_tree = Arc::clone(tree);
    let rows_tree = Arc::clone(tree);
    let empty_tree = Arc::clone(tree);

    let list_header = view()
        .flex_row()
        .items_center()
        .justify_between()
        .pb_1()
        .border_b_1()
        .border_color(palette::BORDER_SUBTLE)
        .child(
            text(move || format!("{} items", state::visible_items(&count_tree).len()))
                .font_family(palette::mono_font())
                .tabular_nums()
                .text_size(px(12.0))
                .text_color(palette::TEXT_MUTED),
        )
        .child(
            text("last used")
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_FAINT),
        );

    let empty_state = || {
        view().py_6().items_center().child(
            text("No matching entries in this scope")
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_FAINT),
        )
    };

    view().flex_1().gap_2().child(list_header).child(
        scroll().flex_1().child(
            view()
                .gap_1()
                .child(each(
                    move || state::explorer_display_items(&items_tree),
                    |id| *id,
                    move |id_sig, _idx_sig| explorer_row(&rows_tree, id_sig),
                ))
                .child(when(
                    move || state::explorer_display_items(&empty_tree).is_empty(),
                    empty_state,
                )),
        ),
    )
}

fn snag_pop_out_motion() -> MotionSpec {
    let timing = KeyframeTiming::new(260.0).easing(Easing::Bezier(0.32, 0.0, 0.67, 0.0));
    MotionSpec::new(timing)
        .track(
            MotionChannel::Scale,
            [
                Keyframe::at(0.0, 1.0),
                Keyframe::at(0.28, 1.03),
                Keyframe::at(1.0, 0.88),
            ],
        )
        .track(
            MotionChannel::TranslateX,
            [
                Keyframe::at(0.0, 0.0),
                Keyframe::at(0.28, 4.0),
                Keyframe::at(1.0, 40.0),
            ],
        )
        .track(
            MotionChannel::Opacity,
            [
                Keyframe::at(0.0, 1.0),
                Keyframe::at(0.25, 0.95),
                Keyframe::at(1.0, 0.0),
            ],
        )
}

fn explorer_row(tree: &Arc<StorageTree>, id_sig: ReadSignal<NodeId>) -> View {
    let node_id = id_sig.get_untracked();
    let row_motion = motion();
    let was_exiting = Rc::new(Cell::new(false));
    let effect_flag = Rc::clone(&was_exiting);

    effect(move || {
        let exiting = state::exiting_signal().with(|e| e.nodes.contains(&node_id));
        if exiting && !effect_flag.get() {
            effect_flag.set(true);
            let _ = row_motion.play(snag_pop_out_motion());
        } else if !exiting && effect_flag.get() {
            effect_flag.set(false);
            row_motion.reset();
        }
    });

    let bg_tree = Arc::clone(tree);
    let content_tree = Arc::clone(tree);

    view()
        .bind_motion(&row_motion)
        .px_2()
        .py(px(6.0))
        .rounded_md()
        .bg(move || {
            let exiting = state::exiting_signal().with(|e| e.nodes.contains(&node_id));
            if exiting {
                palette::WARN_SURFACE
            } else if state::is_row_selected(&bg_tree, node_id) {
                palette::BG_SELECTED
            } else {
                palette::BG_CANVAS
            }
        })
        .child(dynamic(move || {
            explorer_row_contents(&content_tree, node_id)
        }))
}

#[expect(
    clippy::too_many_lines,
    reason = "explorer row coordinates selection, path hint, culprit summary, and inline actions"
)]
fn explorer_row_contents(tree: &Arc<StorageTree>, node_id: NodeId) -> Option<View> {
    let node = tree.node(node_id)?;
    let is_exiting = state::exiting_signal().with(|e| e.nodes.contains(&node_id));
    let is_selected = !is_exiting && state::is_row_selected(tree, node_id);
    let scope_bytes = state::current_scope_bytes(tree).max(1);
    let browse = state::browse_signal().get();
    let is_searching = !browse.search_text.trim().is_empty();

    let row_accent = palette::category_color(node.category);
    let share_pct = (node.total_bytes.saturating_mul(100) / scope_bytes).min(100);
    #[expect(
        clippy::cast_precision_loss,
        reason = "visual dot bar fraction tolerates f32 precision"
    )]
    let share_ratio = (node.total_bytes as f32 / scope_bytes as f32).clamp(0.0, 1.0);
    let age = format_age(node.modified, tree.scanned_at());
    let age_color = match age.freshness {
        AgeFreshness::Stale => palette::WARN_AMBER,
        AgeFreshness::Today | AgeFreshness::Recent | AgeFreshness::Unknown | _ => {
            palette::TEXT_MUTED
        }
    };

    let is_dir = node.kind == EntryKind::Directory;
    let name_display = if is_dir {
        format!("{}/", node.name)
    } else {
        node.name.clone()
    };
    let is_unused = node.junk_kind.is_some();
    let parent_hint = if is_searching {
        search_parent_hint(tree, node, browse.current_node)
    } else {
        None
    };

    let press_tree = Arc::clone(tree);
    let main_row = button()
        .accessibility_label(format!("Item {}", node.name))
        .on_press(move || {
            if state::exiting_signal().with_untracked(|e| e.nodes.contains(&node_id)) {
                return;
            }
            let already = state::is_row_selected_untracked(&press_tree, node_id);
            if already && is_dir {
                state::open_node(node_id);
            } else {
                state::select_visible_node(&press_tree, node_id);
            }
        })
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            view()
                .flex_row()
                .items_center()
                .flex_1()
                .gap_2()
                .child(
                    text(if is_selected { ">" } else { " " })
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_PRIMARY),
                )
                .child(
                    text(if is_dir { "▸" } else { "•" })
                        .font_family(palette::mono_font())
                        .text_size(px(11.0))
                        .text_color(if is_selected {
                            palette::TEXT_PRIMARY
                        } else {
                            palette::TEXT_FAINT
                        }),
                )
                .child(
                    text(name_display)
                        .font_family(palette::mono_font())
                        .font_weight(if is_selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_size(px(13.0))
                        .text_color(if is_selected {
                            palette::TEXT_PRIMARY
                        } else {
                            palette::TEXT_MUTED
                        })
                        .truncate(),
                )
                .child(if is_unused {
                    text("unused")
                        .font_family(palette::mono_font())
                        .text_size(px(11.0))
                        .text_color(palette::WARN_AMBER)
                } else {
                    text("")
                })
                .child(if is_exiting {
                    text("→ queued")
                        .font_family(palette::mono_font())
                        .text_size(px(11.0))
                        .text_color(palette::WARN_AMBER)
                } else {
                    text("")
                }),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    text(format_bytes(node.total_bytes))
                        .font_family(palette::mono_font())
                        .tabular_nums()
                        .text_size(px(12.0))
                        .text_color(if is_selected {
                            palette::TEXT_PRIMARY
                        } else {
                            palette::TEXT_MUTED
                        }),
                )
                .child(palette::mini_dot_bar(share_ratio, row_accent, 10))
                .child(
                    text(format!("{share_pct:>3}%"))
                        .font_family(palette::mono_font())
                        .tabular_nums()
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_FAINT),
                )
                .child(
                    text(age.label.clone())
                        .font_family(palette::mono_font())
                        .tabular_nums()
                        .text_size(px(12.0))
                        .text_color(age_color)
                        .w(px(46.0)),
                ),
        );

    let sub_line = build_row_subline(node, is_selected, &age.label, parent_hint);

    Some(view().child(main_row).child(sub_line))
}

fn search_parent_hint(
    tree: &StorageTree,
    node: &DiskNode,
    scope_node: Option<NodeId>,
) -> Option<String> {
    let parent_rel = node.relative_path.parent()?;
    if parent_rel.as_os_str().is_empty() {
        return None;
    }
    let scope_rel = scope_node
        .and_then(|id| tree.node(id))
        .map_or_else(|| Path::new(""), |n| n.relative_path.as_path());
    if parent_rel == scope_rel {
        return None;
    }
    let shown = if scope_rel.as_os_str().is_empty() {
        parent_rel
    } else {
        parent_rel.strip_prefix(scope_rel).unwrap_or(parent_rel)
    };
    if shown.as_os_str().is_empty() {
        return None;
    }
    Some(format!("in {}/", shown.display()))
}

fn build_row_subline(
    node: &DiskNode,
    is_selected: bool,
    age_label: &str,
    parent_hint: Option<String>,
) -> Option<View> {
    let node_id = node.id;
    let is_dir = node.kind == EntryKind::Directory;
    let hint_line = parent_hint.map(|hint| {
        text(hint)
            .font_family(palette::mono_font())
            .text_size(px(11.0))
            .text_color(palette::TEXT_FAINT)
            .truncate()
    });

    if is_selected {
        let meta_line = is_dir.then(|| {
            let item_total = node.file_count + node.dir_count;
            text(format!("{item_total} items · changed {age_label}"))
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(palette::TEXT_MUTED)
        });

        let culprit_sub = if is_dir {
            node.largest_culprit.as_ref().map(|culprit| {
                text(format!(
                    "└ mostly in {}  {} · {}%",
                    culprit.relative_subpath,
                    format_bytes(culprit.bytes),
                    culprit.share_percent
                ))
                .font_family(palette::mono_font())
                .tabular_nums()
                .text_size(px(11.0))
                .text_color(palette::TEXT_MUTED)
                .truncate()
            })
        } else {
            None
        };

        let has_culprit = is_dir && node.largest_culprit.is_some();
        let actions_bar = selected_actions_bar(node_id, has_culprit);

        return Some(
            view()
                .pl_6()
                .gap_1()
                .pt_1()
                .child(hint_line)
                .child(meta_line)
                .child(culprit_sub)
                .child(actions_bar),
        );
    }

    let culprit_line = node.largest_culprit.as_ref().map(|culprit| {
        text(format!(
            "└ mostly in {}  {} · {}%",
            culprit.relative_subpath,
            format_bytes(culprit.bytes),
            culprit.share_percent
        ))
        .font_family(palette::mono_font())
        .tabular_nums()
        .text_size(px(11.0))
        .text_color(palette::TEXT_FAINT)
        .truncate()
    });

    if hint_line.is_none() && culprit_line.is_none() {
        return None;
    }

    Some(view().pl_6().child(hint_line).child(culprit_line))
}

fn selected_actions_bar(node_id: NodeId, has_culprit: bool) -> View {
    view()
        .flex_row()
        .items_center()
        .flex_wrap()
        .gap_3()
        .pt_1()
        .child(inline_action(
            "→ open",
            palette::TEXT_PRIMARY,
            move || {
                state::open_node(node_id);
            },
        ))
        .child(has_culprit.then(|| {
            inline_action("g go to culprit", palette::TEXT_MUTED, move || {
                state::jump_to_culprit(node_id);
            })
        }))
        .child(inline_action(
            "o reveal in finder",
            palette::TEXT_MUTED,
            move || state::reveal_node(node_id),
        ))
        .child(inline_action("s snag", palette::WARN_AMBER, move || {
            state::snag_node(node_id);
        }))
        .child(inline_action(
            "⌫ move to trash",
            palette::DANGER_RED,
            move || state::quick_trash_node(node_id),
        ))
}

fn inline_action(
    label: &'static str,
    color: nsh_core::prelude::Rgba,
    on_press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .px(px(6.0))
        .py(px(2.0))
        .rounded_sm()
        .bg(palette::BG_INSET)
        .hover(|s| s.bg(palette::BORDER_SUBTLE))
        .cursor(Cursor::Pointer)
        .child(
            text(label)
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(color),
        )
}

fn aggregates_section(tree: Arc<StorageTree>) -> View {
    view()
        .flex_1()
        .child(dynamic(move || build_aggregates_section(&tree)))
}

fn build_aggregates_section(tree: &StorageTree) -> View {
    let browse = state::browse_signal().get();
    let scope = browse.current_node.unwrap_or_else(|| tree.root_id());
    let kind = browse.aggregate_kind;
    let report = compute_aggregates(tree, scope, 40);

    let sub_tabs = view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(agg_tab(
            "By Extension",
            kind == AggregateKind::ByExtension,
            AggregateKind::ByExtension,
        ))
        .child(agg_tab(
            "Top Heavy Folders",
            kind == AggregateKind::HeavyFolders,
            AggregateKind::HeavyFolders,
        ))
        .child(agg_tab(
            "Junk Clusters",
            kind == AggregateKind::JunkClusters,
            AggregateKind::JunkClusters,
        ));

    let list_content = match kind {
        AggregateKind::ByExtension => extension_aggregate_list(report.by_extension),
        AggregateKind::HeavyFolders => aggregate_node_list(tree, report.top_folders),
        AggregateKind::JunkClusters => aggregate_node_list(tree, report.junk_clusters),
    };

    view()
        .flex_1()
        .gap_3()
        .child(sub_tabs)
        .child(scroll().flex_1().child(list_content))
}

fn extension_aggregate_list(by_extension: Vec<disk_space_core::ExtensionAggregate>) -> View {
    let max_ext = by_extension.first().map_or(1, |ext| ext.total_bytes).max(1);
    view()
        .gap_1()
        .children(by_extension.into_iter().map(move |ext| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "visual ratio tolerates f32 precision"
            )]
            let ratio = (ext.total_bytes as f32 / max_ext as f32).clamp(0.0, 1.0);
            let sample = ext.sample_node;
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(
                            text(format!(".{}", ext.extension))
                                .font_family(palette::mono_font())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(12.0))
                                .text_color(palette::TEXT_PRIMARY),
                        )
                        .child(
                            text(format!("{} files", ext.file_count))
                                .font_family(palette::mono_font())
                                .tabular_nums()
                                .text_size(px(11.0))
                                .text_color(palette::TEXT_MUTED),
                        ),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(
                            text(format_bytes(ext.total_bytes))
                                .font_family(palette::mono_font())
                                .tabular_nums()
                                .text_size(px(12.0))
                                .text_color(palette::TEXT_PRIMARY),
                        )
                        .child(palette::mini_dot_bar(
                            ratio,
                            palette::category_color(StorageCategory::Documents),
                            8,
                        ))
                        .child(inline_action(
                            "inspect largest",
                            palette::TEXT_MUTED,
                            move || {
                                state::select_node_in_parent(sample);
                            },
                        )),
                )
        }))
}

fn agg_tab(label: &'static str, active: bool, kind: AggregateKind) -> nsh_base::ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(move || {
            state::browse_signal().update(|b| b.aggregate_kind = kind);
        })
        .px_3()
        .py_1()
        .rounded_md()
        .cursor(Cursor::Pointer)
        .bg(if active {
            palette::BG_SELECTED
        } else {
            palette::BG_SURFACE
        })
        .border_1()
        .border_color(if active {
            palette::BORDER_ACTIVE
        } else {
            palette::BORDER_SUBTLE
        })
        .child(
            text(label)
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(if active {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }),
        )
}

fn aggregate_node_list(tree: &StorageTree, ids: Vec<NodeId>) -> View {
    view().gap_1().children(ids.into_iter().filter_map(|id| {
        let node = tree.node(id)?;
        let covered = state::snag_signal().with(|s| s.queue.is_covered(&node.relative_path));
        if covered {
            return None;
        }
        let accent = palette::category_color(node.category);
        let rel = node.relative_path.display().to_string();

        Some(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .gap_3()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .flex_1()
                        .gap_2()
                        .child(view().w(px(7.0)).h(px(7.0)).rounded(px(1.5)).bg(accent))
                        .child(
                            text(rel)
                                .font_family(palette::mono_font())
                                .text_size(px(12.0))
                                .text_color(palette::TEXT_PRIMARY)
                                .truncate(),
                        ),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            text(format_bytes(node.total_bytes))
                                .font_family(palette::mono_font())
                                .tabular_nums()
                                .text_size(px(12.0))
                                .text_color(palette::TEXT_MUTED),
                        )
                        .child(inline_action("open", palette::TEXT_MUTED, move || {
                            state::select_node_in_parent(id);
                        }))
                        .child(inline_action("+ snag", palette::WARN_AMBER, move || {
                            state::snag_node(id);
                        })),
                ),
        )
    }))
}
