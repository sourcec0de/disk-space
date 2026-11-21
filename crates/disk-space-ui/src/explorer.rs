//! Main explorer pane: breadcrumbs, search & junk filters, item rows, and aggregates.

use disk_space_core::AgeFreshness;
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
use nsh_core::prelude::ComponentBuilder;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::px;
use nsh_core::prelude::scroll;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::palette;
use crate::state;
use crate::state::AggregateKind;
use crate::state::ViewTab;
use crate::sunburst;

/// Renders the right detail pane (breadcrumbs, search/filter strip, rows or aggregates).
#[expect(
    clippy::too_many_lines,
    reason = "explorer coordinates breadcrumb header, filter strip, and list items"
)]
pub(crate) fn explorer_pane(tree: &StorageTree) -> impl IntoElement {
    let browse = state::browse_signal().get();
    let header_node_id = browse.current_node.unwrap_or_else(|| tree.root_id());
    let header_node = tree.node(header_node_id);

    let (header_bytes, unused_bytes, review_first) =
        match (browse.current_node, browse.selected_category) {
            (None, Some(cat)) => {
                let root_node = tree.node(tree.root_id());
                let cat_bytes = root_node.map_or(0, |n| n.category_totals.get(cat));
                let cat_unused: u64 = tree
                    .category_entries(cat)
                    .iter()
                    .filter_map(|id| tree.node(*id))
                    .map(|n| n.unused_bytes)
                    .sum();
                (cat_bytes, cat_unused, false)
            }
            _ => (
                header_node.map_or(0, |n| n.total_bytes),
                header_node.map_or(0, |n| n.unused_bytes),
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

    let top_header = view()
        .gap_1()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_3()
                .child(breadcrumbs)
                .child(
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
                        }),
                ),
        )
        .child(
            text(subtitle_path)
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_FAINT)
                .truncate(),
        );

    let search_bar = view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            input()
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
        .child((!browse.search_text.is_empty()).then(|| {
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
        }));

    let junk_only = browse.filter_mode == FilterMode::JunkAndUnusedOnly;
    let filter_strip = view()
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
        .child(
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
                            text(match browse.sort_order {
                                SortOrder::AgeOldestFirst => "sort: age",
                                SortOrder::NameAsc => "sort: name",
                                SortOrder::SizeDesc | _ => "sort: size",
                            })
                            .font_family(palette::mono_font())
                            .text_size(px(11.0))
                            .text_color(palette::TEXT_MUTED),
                        ),
                ),
        );

    let main_body = match browse.view_tab {
        ViewTab::Sunburst => view()
            .flex_1()
            .gap_3()
            .child(sunburst::sunburst_panel(tree))
            .child(item_list_section(tree, header_bytes, active_accent)),
        ViewTab::Aggregates => aggregates_section(tree, header_node_id, browse.aggregate_kind),
        ViewTab::DotMatrix => item_list_section(tree, header_bytes, active_accent),
    };

    view()
        .flex_1()
        .gap_3()
        .child(top_header)
        .child(search_bar)
        .child(filter_strip)
        .child(main_body)
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

#[expect(
    clippy::too_many_lines,
    reason = "item rows carry inline culprit lines and contextual action bar"
)]
fn item_list_section(
    tree: &StorageTree,
    scope_bytes: u64,
    _active_accent: nsh_core::prelude::Rgba,
) -> View {
    let items = state::visible_items(tree);
    let browse = state::browse_signal().get();
    let total_count = items.len();
    let clamped_selected = if total_count == 0 {
        0
    } else {
        browse.selected_row.min(total_count - 1)
    };
    let denom = scope_bytes.max(1);
    let now = tree.scanned_at();

    let list_header = view()
        .flex_row()
        .items_center()
        .justify_between()
        .pb_1()
        .border_b_1()
        .border_color(palette::BORDER_SUBTLE)
        .child(
            text(format!("{total_count} items"))
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

    let rows = items
        .into_iter()
        .enumerate()
        .filter_map(move |(idx, node_id)| {
            let node = tree.node(node_id)?;
            let is_selected = idx == clamped_selected;
            let row_accent = palette::category_color(node.category);
            let is_snagged =
                state::snag_signal().with(|snag| snag.queue.is_snagged(&node.relative_path));
            let is_covered =
                state::snag_signal().with(|snag| snag.queue.is_covered(&node.relative_path));

            let share_pct = (node.total_bytes.saturating_mul(100) / denom).min(100);
            #[expect(
                clippy::cast_precision_loss,
                reason = "visual dot bar fraction tolerates f32 precision"
            )]
            let share_ratio = (node.total_bytes as f32 / denom as f32).clamp(0.0, 1.0);
            let age = format_age(node.modified, now);
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

            let culprit_line = if is_selected && is_dir {
                let item_total = node.file_count + node.dir_count;
                let meta_line = text(format!("{item_total} items · changed {}", age.label))
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::TEXT_MUTED);

                let culprit_sub = node.largest_culprit.as_ref().map(|culprit| {
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
                });

                let has_culprit = node.largest_culprit.is_some();
                let actions_bar = view()
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
                    .child(inline_action(
                        if is_snagged { "s unsnag" } else { "s snag" },
                        if is_snagged {
                            palette::DANGER_RED
                        } else {
                            palette::WARN_AMBER
                        },
                        move || state::toggle_snag_node(node_id),
                    ))
                    .child(inline_action(
                        "⌫ move to trash",
                        palette::DANGER_RED,
                        move || state::quick_trash_node(node_id),
                    ));

                Some(
                    view()
                        .pl_6()
                        .gap_1()
                        .pt_1()
                        .child(meta_line)
                        .child(culprit_sub)
                        .child(actions_bar),
                )
            } else if is_selected {
                let actions_bar = view()
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
                    .child(inline_action(
                        "o reveal in finder",
                        palette::TEXT_MUTED,
                        move || state::reveal_node(node_id),
                    ))
                    .child(inline_action(
                        if is_snagged { "s unsnag" } else { "s snag" },
                        if is_snagged {
                            palette::DANGER_RED
                        } else {
                            palette::WARN_AMBER
                        },
                        move || state::toggle_snag_node(node_id),
                    ))
                    .child(inline_action(
                        "⌫ move to trash",
                        palette::DANGER_RED,
                        move || state::quick_trash_node(node_id),
                    ));
                Some(view().pl_6().gap_1().pt_1().child(actions_bar))
            } else {
                node.largest_culprit.as_ref().map(|culprit| {
                    view().pl_6().child(
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
                        .truncate(),
                    )
                })
            };

            let main_row = button()
                .accessibility_label(format!("Item {}", node.name))
                .on_press(move || {
                    let already = state::browse_signal().with_untracked(|b| b.selected_row == idx);
                    if already && is_dir {
                        state::open_node(node_id);
                    } else {
                        state::browse_signal().update(|b| b.selected_row = idx);
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
                        .child(if is_covered {
                            text("snagged")
                                .font_family(palette::mono_font())
                                .text_size(px(11.0))
                                .text_color(palette::DANGER_RED)
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
                            text(age.label)
                                .font_family(palette::mono_font())
                                .tabular_nums()
                                .text_size(px(12.0))
                                .text_color(age_color)
                                .w(px(46.0)),
                        ),
                );

            Some(
                view()
                    .px_2()
                    .py(px(6.0))
                    .rounded_md()
                    .bg(if is_selected {
                        palette::BG_SELECTED
                    } else {
                        palette::BG_CANVAS
                    })
                    .child(main_row)
                    .child(culprit_line),
            )
        });

    let empty_state = if total_count == 0 {
        Some(
            view().py_6().items_center().child(
                text("No matching entries in this scope")
                    .font_family(palette::mono_font())
                    .text_size(px(12.0))
                    .text_color(palette::TEXT_FAINT),
            ),
        )
    } else {
        None
    };

    view().flex_1().gap_2().child(list_header).child(
        scroll()
            .flex_1()
            .child(view().gap_1().children(rows).child(empty_state)),
    )
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

fn aggregates_section(tree: &StorageTree, scope: NodeId, kind: AggregateKind) -> View {
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
        let accent = palette::category_color(node.category);
        let rel = node.relative_path.display().to_string();
        let is_snagged = state::snag_signal().with(|s| s.queue.is_snagged(&node.relative_path));

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
                        .child(inline_action(
                            if is_snagged { "unsnag" } else { "+ snag" },
                            if is_snagged {
                                palette::DANGER_RED
                            } else {
                                palette::WARN_AMBER
                            },
                            move || state::toggle_snag_node(id),
                        )),
                ),
        )
    }))
}
