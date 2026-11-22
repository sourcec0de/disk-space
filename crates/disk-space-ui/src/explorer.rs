//! Main explorer pane: breadcrumbs, search & junk filters, virtualized accessible listbox, and aggregates.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use disk_space_core::AgeFreshness;
use disk_space_core::FilterMode;
use disk_space_core::NodeId;
use disk_space_core::NodeRef;
use disk_space_core::SortOrder;
use disk_space_core::StorageTree;
use disk_space_core::compute_aggregates;
use disk_space_core::format_age;
use disk_space_core::format_bytes;
use disk_space_platform::EntryKind;
use disk_space_platform::StorageCategory;
use nsh_base::ButtonElement;
use nsh_base::button;
use nsh_base::input;
use nsh_core::keymap::When;
use nsh_core::prelude::AccessibilityRole;
use nsh_core::prelude::AccessibilityState;
use nsh_core::prelude::Accessible;
use nsh_core::prelude::ComponentBuilder;
use nsh_core::prelude::Composition;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::Easing;
use nsh_core::prelude::Element;
use nsh_core::prelude::ElementRef;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::Interactive;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::KeyOptions;
use nsh_core::prelude::Keyframe;
use nsh_core::prelude::KeyframeTiming;
use nsh_core::prelude::ListLayout;
use nsh_core::prelude::Memo;
use nsh_core::prelude::MotionChannel;
use nsh_core::prelude::MotionSpec;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Percentage;
use nsh_core::prelude::PointerEvents;
use nsh_core::prelude::Position;
use nsh_core::prelude::PressEvent;
use nsh_core::prelude::Signal;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::ViewableItems;
use nsh_core::prelude::announce_for_accessibility;
use nsh_core::prelude::batch;
use nsh_core::prelude::computed;
use nsh_core::prelude::dynamic;
use nsh_core::prelude::effect;
use nsh_core::prelude::element_ref;
use nsh_core::prelude::motion;
use nsh_core::prelude::on_key_with;
use nsh_core::prelude::px;
use nsh_core::prelude::reads_active_descendants;
use nsh_core::prelude::rgba;
use nsh_core::prelude::screen_reader;
use nsh_core::prelude::scroll;
use nsh_core::prelude::signal;
use nsh_core::prelude::text;
use nsh_core::prelude::untracked;
use nsh_core::prelude::view;
use nsh_core::prelude::virtual_list;
use nsh_core::prelude::when;

use crate::keybindings::Action;
use crate::palette;
use crate::state;
use crate::state::AggregateKind;
use crate::state::ListMove;
use crate::state::SelectionState;
use crate::state::ViewTab;
use crate::sunburst;

const DEFAULT_PAGE_ROWS: usize = 10;

/// Controller owning the explorer's memoized list model, focus refs, and keyboard actions.
#[derive(Clone)]
struct ExplorerController {
    tree: Arc<StorageTree>,
    data: state::ExplorerListData,
    scope_node: Memo<Option<NodeId>>,
    is_searching: Memo<bool>,
    scope_bytes: Memo<u64>,
    multi_selected: Memo<bool>,
    search_ref: ElementRef,
    listbox_ref: ElementRef,
    list_ref: ElementRef,
    search_focused: Signal<bool>,
    list_focused: Signal<bool>,
    active_row: Signal<Option<(NodeId, ElementRef)>>,
    viewable_range: Rc<Cell<Option<(usize, usize)>>>,
}

impl ExplorerController {
    fn new(tree: Arc<StorageTree>) -> Self {
        let data = state::ExplorerListData::new(Arc::clone(&tree));
        let scope_node = computed(|| state::browse_signal().with(|b| b.current_node));
        let is_searching =
            computed(|| state::browse_signal().with(|b| !b.search_text.trim().is_empty()));
        let scope_tree = Arc::clone(&tree);
        let scope_bytes = computed(move || state::current_scope_bytes(&scope_tree));
        let multi_selected =
            computed(|| state::selection_signal().with(|sel| sel.selected.len() > 1));

        // Reconcile cursor & selection whenever the visible item set changes or selection was cleared.
        let visible_watch = data.visible_items;
        let no_cursor = computed(|| state::selection_signal().with(|sel| sel.cursor.is_none()));
        effect(move || {
            let _missing = no_cursor.get();
            visible_watch.with(|visible| {
                untracked(|| {
                    let prev = state::selection_signal().get_untracked();
                    let mut next = prev.clone();
                    next.reconcile(visible);
                    if next != prev {
                        state::selection_signal().set(next);
                    }
                });
            });
        });

        Self {
            tree,
            data,
            scope_node,
            is_searching,
            scope_bytes,
            multi_selected,
            search_ref: element_ref(),
            listbox_ref: element_ref(),
            list_ref: element_ref(),
            search_focused: signal(false),
            list_focused: signal(false),
            active_row: signal(None),
            viewable_range: Rc::new(Cell::new(None)),
        }
    }

    fn active_row_ref(&self) -> Option<ElementRef> {
        let cursor = state::selection_signal().with(|sel| sel.cursor)?;
        self.active_row
            .get()
            .filter(|&(id, _)| id == cursor)
            .map(|(_, r)| r)
    }

    fn page_size(&self) -> usize {
        self.viewable_range
            .get()
            .map_or(DEFAULT_PAGE_ROWS, |(first, last)| {
                last.saturating_sub(first).saturating_add(1)
            })
            .max(1)
    }

    fn reveal_index(&self, idx: usize) {
        if let Some((first, last)) = self.viewable_range.get() {
            if (first..=last).contains(&idx) {
                return;
            }
            let pos = if idx < first {
                Percentage(0.0)
            } else {
                Percentage(1.0)
            };
            if let Ok(index) = u32::try_from(idx) {
                self.list_ref.scroll_to_index(index, pos, px(0.0), false);
            }
        } else if let Ok(index) = u32::try_from(idx) {
            self.list_ref
                .scroll_to_index(index, Percentage(0.0), px(0.0), false);
        }
    }

    fn announce_cursor(&self, node_id: NodeId) {
        if !untracked(screen_reader) || reads_active_descendants() {
            return;
        }
        let Some(node) = self.tree.node(node_id) else {
            return;
        };
        let count = state::selection_signal().with_untracked(|sel| sel.selected.len());
        let msg = if count > 1 {
            format!(
                "{}, {}, {} selected",
                node.name(),
                format_bytes(node.total_bytes()),
                count
            )
        } else {
            format!("{}, {}", node.name(), format_bytes(node.total_bytes()))
        };
        announce_for_accessibility(msg);
    }

    fn enter_list_from_search(&self) {
        self.search_ref.blur();
        self.listbox_ref.focus();
        self.data.visible_items.with_untracked(|visible| {
            let Some(&first) = visible.first() else {
                return;
            };
            let target = state::selection_signal().with_untracked(|sel| sel.cursor);
            let idx = target
                .and_then(|id| visible.iter().position(|&v| v == id))
                .unwrap_or_else(|| {
                    state::selection_signal().update(|sel| sel.select_single(first));
                    0
                });
            self.reveal_index(idx);
            if let Some(&id) = visible.get(idx) {
                self.announce_cursor(id);
            }
        });
    }

    fn move_selection(&self, step: ListMove, extend: bool) {
        self.data.visible_items.with_untracked(|visible| {
            if visible.is_empty() {
                return;
            }
            let mut next_idx = None;
            state::selection_signal().update(|sel| {
                next_idx = sel.move_cursor(visible, step, extend);
            });
            if !self.list_focused.get_untracked() {
                self.listbox_ref.focus();
            }
            if let Some(idx) = next_idx {
                self.reveal_index(idx);
                if let Some(&id) = visible.get(idx) {
                    self.announce_cursor(id);
                }
            }
        });
    }

    fn select_range_to(&self, target: NodeId) {
        self.data.visible_items.with_untracked(|visible| {
            let mut idx = None;
            state::selection_signal().update(|sel| {
                idx = sel.select_range_to(visible, target);
            });
            if let Some(i) = idx {
                self.reveal_index(i);
            }
        });
    }

    fn select_all(&self) {
        self.data.visible_items.with_untracked(|visible| {
            state::selection_signal().update(|sel| sel.select_all(visible));
        });
    }

    fn snag_selected(&self) {
        self.data.visible_items.with_untracked(|visible| {
            let targets =
                state::selection_signal().with_untracked(|sel| sel.selected_in_order(visible));
            if targets.is_empty() {
                return;
            }
            self.data.display_items.with_untracked(|display| {
                state::snag_nodes_in_display(&targets, display);
            });
        });
    }

    fn cursor_node_id() -> Option<NodeId> {
        state::selection_signal().with_untracked(|sel| sel.cursor)
    }

    fn focus_search(&self) {
        self.search_ref.focus();
    }
}

fn can_handle_list_keys() -> bool {
    let in_aggregates =
        state::browse_signal().with_untracked(|b| b.view_tab == ViewTab::Aggregates);
    state::can_dispatch_app_shortcut() && !in_aggregates
}

fn bind_list_shortcuts(ctrl: &ExplorerController) {
    let search_ctrl = ctrl.clone();
    effect(move || {
        if !search_ctrl.search_focused.get() {
            return;
        }
        let enter_ctrl = search_ctrl.clone();
        on_key_with(
            "down",
            KeyOptions {
                when: When::Always,
                repeat: false,
                for_focus: true,
                ..KeyOptions::default()
            },
            move |_| {
                if !state::snag_signal().with_untracked(|s| s.confirm_open) {
                    enter_ctrl.enter_list_from_search();
                }
            },
        );
        let esc_ctrl = search_ctrl.clone();
        on_key_with(
            "escape",
            KeyOptions {
                when: When::Always,
                repeat: false,
                for_focus: true,
                ..KeyOptions::default()
            },
            move |_| {
                let has_query =
                    state::browse_signal().with_untracked(|b| !b.search_text.is_empty());
                if has_query {
                    batch(|| {
                        state::browse_signal().update(|b| b.search_text.clear());
                        state::selection_signal().update(SelectionState::clear);
                    });
                } else {
                    esc_ctrl.enter_list_from_search();
                }
            },
        );
    });

    let list_focus = ctrl.list_focused;
    effect(move || {
        let focused = list_focus.get();
        if !focused {
            return;
        }
        on_key_with(
            "enter",
            KeyOptions {
                when: When::NotEditing,
                repeat: false,
                for_focus: true,
                ..KeyOptions::default()
            },
            |_| {
                if can_handle_list_keys()
                    && let Some(id) = ExplorerController::cursor_node_id()
                {
                    state::open_node(id);
                }
            },
        );
    });

    bind_motion_keys(ctrl);
    bind_action_keys(ctrl);
}

fn bind_motion_keys(ctrl: &ExplorerController) {
    let repeat_opts = KeyOptions {
        when: When::NotEditing,
        repeat: true,
        ..KeyOptions::default()
    };

    for (key, step, extend) in [
        ("shift+down", ListMove::Down, true),
        ("shift+j", ListMove::Down, true),
        ("shift+up", ListMove::Up, true),
        ("shift+k", ListMove::Up, true),
        ("home", ListMove::First, false),
        ("end", ListMove::Last, false),
        ("shift+home", ListMove::First, true),
        ("shift+end", ListMove::Last, true),
    ] {
        let move_ctrl = ctrl.clone();
        on_key_with(key, repeat_opts, move |_| {
            if can_handle_list_keys() {
                move_ctrl.move_selection(step, extend);
            }
        });
    }

    for (key, dir, extend) in [
        ("pageup", -1_isize, false),
        ("pagedown", 1_isize, false),
        ("shift+pageup", -1_isize, true),
        ("shift+pagedown", 1_isize, true),
    ] {
        let page_ctrl = ctrl.clone();
        on_key_with(key, repeat_opts, move |_| {
            if !can_handle_list_keys() {
                return;
            }
            let page = page_ctrl.page_size();
            let step = if dir < 0 {
                ListMove::PageUp(page)
            } else {
                ListMove::PageDown(page)
            };
            page_ctrl.move_selection(step, extend);
        });
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "registers all reactive per-action list shortcuts inside one effect"
)]
fn bind_action_keys(ctrl: &ExplorerController) {
    let reactive_ctrl = ctrl.clone();
    effect(move || {
        let kb = state::keybindings_signal().get();
        let repeat_opts = KeyOptions {
            when: When::NotEditing,
            repeat: true,
            ..KeyOptions::default()
        };
        let once_opts = KeyOptions {
            when: When::NotEditing,
            repeat: false,
            ..KeyOptions::default()
        };

        for key in kb.key_strings_for(Action::MoveDown) {
            let move_ctrl = reactive_ctrl.clone();
            on_key_with(key, repeat_opts, move |_| {
                if can_handle_list_keys() {
                    move_ctrl.move_selection(ListMove::Down, false);
                }
            });
        }
        for key in kb.key_strings_for(Action::MoveUp) {
            let move_ctrl = reactive_ctrl.clone();
            on_key_with(key, repeat_opts, move |_| {
                if can_handle_list_keys() {
                    move_ctrl.move_selection(ListMove::Up, false);
                }
            });
        }
        for key in kb.key_strings_for(Action::SelectAll) {
            let select_all_ctrl = reactive_ctrl.clone();
            on_key_with(key, once_opts, move |_| {
                if can_handle_list_keys() {
                    select_all_ctrl.select_all();
                }
            });
        }
        for key in kb.key_strings_for(Action::SnagSelected) {
            let snag_ctrl = reactive_ctrl.clone();
            on_key_with(key, once_opts, move |_| {
                if can_handle_list_keys() {
                    snag_ctrl.snag_selected();
                }
            });
        }
        for key in kb.key_strings_for(Action::QuickLook) {
            on_key_with(key, once_opts, |_| {
                if can_handle_list_keys()
                    && let Some(id) = ExplorerController::cursor_node_id()
                {
                    state::preview_node(id);
                }
            });
        }
        for key in kb.key_strings_for(Action::OpenSelected) {
            on_key_with(key, once_opts, |_| {
                if can_handle_list_keys()
                    && let Some(id) = ExplorerController::cursor_node_id()
                {
                    state::open_node(id);
                }
            });
        }
        for key in kb.key_strings_for(Action::AscendDirectory) {
            on_key_with(key, once_opts, |_| {
                if can_handle_list_keys() {
                    state::ascend_directory();
                }
            });
        }
        for key in kb.key_strings_for(Action::JumpToCulprit) {
            on_key_with(key, once_opts, |_| {
                if can_handle_list_keys()
                    && let Some(id) = ExplorerController::cursor_node_id()
                {
                    state::jump_to_culprit(id);
                }
            });
        }
        for key in kb.key_strings_for(Action::RevealInFinder) {
            on_key_with(key, once_opts, |_| {
                if can_handle_list_keys()
                    && let Some(id) = ExplorerController::cursor_node_id()
                {
                    state::reveal_node(id);
                }
            });
        }
        for key in kb.key_strings_for(Action::FocusSearch) {
            let has_mod = key.contains("mod+")
                || key.contains("cmd+")
                || key.contains("ctrl+")
                || key.contains("alt+");
            let search_ctrl = reactive_ctrl.clone();
            on_key_with(
                key,
                KeyOptions {
                    when: if has_mod {
                        When::Always
                    } else {
                        When::NotEditing
                    },
                    repeat: false,
                    ..KeyOptions::default()
                },
                move |_| {
                    if state::can_dispatch_app_shortcut() {
                        search_ctrl.focus_search();
                    }
                },
            );
        }
    });
}

/// Renders the right detail pane (breadcrumbs, search/filter strip, rows or aggregates).
pub(crate) fn explorer_pane(tree: &Arc<StorageTree>) -> impl IntoElement {
    let ctrl = ExplorerController::new(Arc::clone(tree));
    bind_list_shortcuts(&ctrl);

    let header_key =
        computed(|| state::browse_signal().with(|b| (b.current_node, b.selected_category)));
    let filter_key = computed(|| {
        state::browse_signal().with(|b| {
            (
                b.current_node,
                b.selected_category,
                b.filter_mode,
                b.sort_order,
            )
        })
    });
    let active_tab = computed(|| state::browse_signal().with(|b| b.view_tab));

    let header_tree = Arc::clone(tree);
    let filter_tree = Arc::clone(tree);
    let body_ctrl = ctrl.clone();

    let main_body = dynamic(move || {
        let tab = active_tab.get();
        match tab {
            ViewTab::Sunburst => {
                let sun_tree = Arc::clone(&body_ctrl.tree);
                let active_node = body_ctrl.scope_node;
                view()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap_3()
                    .child(dynamic(move || {
                        sunburst::sunburst_panel(&sun_tree, active_node.get())
                    }))
                    .child(item_list_section(&body_ctrl))
            }
            ViewTab::Aggregates => aggregates_section(Arc::clone(&body_ctrl.tree)),
            ViewTab::DotMatrix => item_list_section(&body_ctrl),
        }
    });

    view()
        .flex_1()
        .min_h(px(0.0))
        .gap_3()
        .child(dynamic(move || {
            let (curr, cat) = header_key.get();
            build_top_header(&header_tree, curr, cat)
        }))
        .child(build_search_bar(&ctrl))
        .child(dynamic(move || {
            let (curr, cat, filter_mode, sort_order) = filter_key.get();
            build_filter_strip(&filter_tree, curr, cat, filter_mode, sort_order)
        }))
        .child(main_body)
}

fn build_top_header(
    tree: &StorageTree,
    current_node: Option<NodeId>,
    selected_category: Option<StorageCategory>,
) -> View {
    let header_node_id = current_node.unwrap_or_else(|| tree.root_id());
    let header_node = tree.node(header_node_id);

    let (header_bytes, review_first) = match (current_node, selected_category) {
        (None, Some(cat)) => {
            let root_node = tree.node(tree.root_id());
            (root_node.map_or(0, |n| n.category_bytes(cat)), false)
        }
        _ => (
            header_node.map_or(0, NodeRef::total_bytes),
            header_node.is_some_and(NodeRef::review_first),
        ),
    };

    let vol_total = tree.volume().total_bytes.max(1);
    let pct_of_disk = header_bytes.saturating_mul(100) / vol_total;
    let active_accent = selected_category
        .or_else(|| header_node.map(NodeRef::category))
        .map_or(palette::TEXT_PRIMARY, palette::category_color);

    let subtitle_path = match (current_node, selected_category) {
        (None, Some(cat)) => format!(
            "{} · {} category view",
            state::display_abs_path(tree.root().canonical_path()),
            cat.label()
        ),
        (Some(id), _) => state::display_abs_path(&tree.absolute_path(id)),
        (None, None) => state::display_abs_path(tree.root().canonical_path()),
    };

    let breadcrumbs = build_breadcrumbs(tree, selected_category, current_node, active_accent);

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

fn build_search_bar(ctrl: &ExplorerController) -> View {
    let composing = signal(false);
    let search_focus = ctrl.search_focused;
    let is_searching = ctrl.is_searching;

    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            input()
                .label("Search storage")
                .element_ref(&ctrl.search_ref)
                .accessibility_controls(Some(ctrl.listbox_ref))
                .value(move || state::browse_signal().with(|b| b.search_text.clone()))
                .on_composition_change(move |comp: &Composition| {
                    composing.set(comp.composing);
                })
                .on_focus(move || search_focus.set(true))
                .on_blur(move || search_focus.set(false))
                .on_change(move |value: &str| {
                    if composing.get_untracked() {
                        return;
                    }
                    batch(|| {
                        state::browse_signal().update(|b| {
                            value.clone_into(&mut b.search_text);
                        });
                        state::selection_signal().update(SelectionState::clear);
                    });
                })
                .placeholder("/ search name, subpath, or .ext... (Down jumps to list)")
                .flex_1()
                .px_3()
                .py(px(6.0))
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .bg(palette::BG_INSET)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .focus(|s| s.border_color(palette::BORDER_ACTIVE))
                .rounded_md()
                .text_color(palette::TEXT_PRIMARY)
                .placeholder_color(palette::TEXT_FAINT),
        )
        .child(when(move || is_searching.get(), clear_search_btn))
}

fn clear_search_btn() -> ButtonElement {
    button()
        .accessibility_label("Clear search")
        .on_press(|| {
            batch(|| {
                state::browse_signal().update(|b| {
                    b.search_text.clear();
                });
                state::selection_signal().update(SelectionState::clear);
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

fn build_filter_strip(
    tree: &StorageTree,
    current_node: Option<NodeId>,
    selected_category: Option<StorageCategory>,
    filter_mode: FilterMode,
    sort_order: SortOrder,
) -> View {
    let header_node_id = current_node.unwrap_or_else(|| tree.root_id());
    let unused_bytes = match (current_node, selected_category) {
        (None, Some(cat)) => tree
            .category_entries(cat)
            .iter()
            .filter_map(|id| tree.node(*id))
            .map(NodeRef::unused_bytes)
            .sum(),
        _ => tree.node(header_node_id).map_or(0, NodeRef::unused_bytes),
    };

    let junk_only = filter_mode == FilterMode::JunkAndUnusedOnly;
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
        .child(filter_controls(junk_only, sort_order))
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
                    batch(|| {
                        state::browse_signal().update(|b| {
                            b.filter_mode = match b.filter_mode {
                                FilterMode::All => FilterMode::JunkAndUnusedOnly,
                                FilterMode::JunkAndUnusedOnly | _ => FilterMode::All,
                            };
                        });
                        state::selection_signal().update(SelectionState::clear);
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
                    batch(|| {
                        state::browse_signal().update(|b| {
                            b.sort_order = match b.sort_order {
                                SortOrder::SizeDesc => SortOrder::AgeOldestFirst,
                                SortOrder::AgeOldestFirst => SortOrder::NameAsc,
                                SortOrder::NameAsc | _ => SortOrder::SizeDesc,
                            };
                        });
                        state::selection_signal().update(SelectionState::clear);
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
                    crumbs.push((format!("/{}", node.name()), Some(id), is_last));
                }
            }
        }
        (None, Some(curr)) => {
            let chain = tree.ancestors(curr);
            for (idx, id) in chain.iter().copied().enumerate() {
                if let Some(node) = tree.node(id) {
                    let label = if idx == 0 {
                        node.name().to_owned()
                    } else {
                        format!("/{}", node.name())
                    };
                    let is_last = idx + 1 == chain.len();
                    crumbs.push((label, Some(id), is_last));
                }
            }
        }
        (None, None) => {
            let root_name = tree
                .node(tree.root_id())
                .map_or_else(|| "root".to_owned(), |n| n.name().to_owned());
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
                    state::navigate_to_node(target);
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

fn item_list_section(ctrl: &ExplorerController) -> View {
    let display = ctrl.data.display_items;
    let empty_state = || {
        view().py_6().items_center().child(
            text("No matching entries in this scope")
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_FAINT),
        )
    };

    let row_ctrl = ctrl.clone();
    let viewable_store = Rc::clone(&ctrl.viewable_range);
    let active_ctrl = ctrl.clone();
    let list_focus = ctrl.list_focused;

    let list_container = view()
        .flex_1()
        .min_h(px(0.0))
        .role(AccessibilityRole::Listbox)
        .label("Storage items")
        .focusable(true)
        .accessibility_active_descendant(move || active_ctrl.active_row_ref())
        .element_ref(&ctrl.listbox_ref)
        .on_focus(move || list_focus.set(true))
        .on_blur(move || list_focus.set(false))
        .rounded_md()
        .focus_visible(|s| s.outline(px(2.0)).outline_color(palette::BORDER_ACTIVE))
        .child(
            virtual_list(
                move || display.with(|d| d.len()),
                move |at| explorer_row_cell(&row_ctrl, at),
            )
            .keys(move || display.with(|d| d.iter().map(|id| id.index()).collect::<Vec<_>>()))
            .estimated_item_size(px(46.0))
            .row_gap(px(4.0))
            .viewability(nsh_core::prelude::Viewability {
                item_visible_threshold: Some(Percentage(0.995)),
                ..nsh_core::prelude::Viewability::default()
            })
            .on_viewable_items_change(move |v: &ViewableItems| {
                viewable_store.set(v.viewable);
            })
            .empty(empty_state())
            .element_ref(&ctrl.list_ref)
            .flex_1(),
        );

    view()
        .flex_1()
        .min_h(px(0.0))
        .gap_2()
        .child(build_list_header(ctrl))
        .child(list_container)
}

fn list_header_selection_summary(
    tree: Arc<StorageTree>,
    visible: Memo<Arc<Vec<NodeId>>>,
    multi_selected: Memo<bool>,
) -> View {
    let selected_info = computed(move || {
        visible.with(|vis| {
            state::selection_signal().with(|sel| {
                let count = sel.selected.len();
                let bytes: u64 = vis
                    .iter()
                    .filter(|id| sel.selected.contains(id))
                    .filter_map(|id| tree.node(*id))
                    .map(NodeRef::total_bytes)
                    .sum();
                (count, bytes)
            })
        })
    });

    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            text(move || format!("{} items", visible.with(|v| v.len())))
                .font_family(palette::mono_font())
                .tabular_nums()
                .text_size(px(12.0))
                .text_color(palette::TEXT_MUTED),
        )
        .child(when(
            move || multi_selected.get(),
            move || {
                view()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        text("·")
                            .font_family(palette::mono_font())
                            .text_size(px(12.0))
                            .text_color(palette::TEXT_FAINT),
                    )
                    .child(
                        text(move || format!("{} selected", selected_info.get().0))
                            .font_family(palette::mono_font())
                            .tabular_nums()
                            .text_size(px(12.0))
                            .text_color(palette::WARN_AMBER),
                    )
                    .child(
                        text(move || {
                            format!(
                                "({}) · press s to snag",
                                format_bytes(selected_info.get().1)
                            )
                        })
                        .font_family(palette::mono_font())
                        .tabular_nums()
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_MUTED),
                    )
            },
        ))
}

fn build_list_header(ctrl: &ExplorerController) -> View {
    let tree = Arc::clone(&ctrl.tree);
    let visible = ctrl.data.visible_items;
    let multi_selected = ctrl.multi_selected;
    let snag_ctrl = ctrl.clone();

    view()
        .flex_row()
        .items_center()
        .justify_between()
        .pb_1()
        .border_b_1()
        .border_color(palette::BORDER_SUBTLE)
        .child(list_header_selection_summary(tree, visible, multi_selected))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(when(
                    move || multi_selected.get(),
                    move || {
                        let bulk_ctrl = snag_ctrl.clone();
                        view()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(inline_action(
                                "s snag selected",
                                palette::WARN_AMBER,
                                move || {
                                    bulk_ctrl.snag_selected();
                                },
                            ))
                            .child(inline_action("esc clear", palette::TEXT_MUTED, || {
                                state::selection_signal()
                                    .update(SelectionState::collapse_to_cursor);
                            }))
                    },
                ))
                .child(
                    text("last used")
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_FAINT),
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

fn bind_row_motion(
    row_id: Memo<Option<NodeId>>,
    is_exiting: Memo<bool>,
) -> nsh_core::prelude::Motion {
    let row_motion = motion();
    let exiting_id = Rc::new(Cell::new(None::<NodeId>));
    effect(move || {
        let cur = row_id.get();
        let exiting = is_exiting.get();
        if exiting {
            if let Some(id) = cur
                && exiting_id.get() != Some(id)
            {
                exiting_id.set(Some(id));
                let _ = row_motion.play(snag_pop_out_motion());
            }
        } else if exiting_id.replace(None).is_some() {
            row_motion.reset();
        }
    });
    row_motion
}

fn bind_active_row(
    ctrl: &ExplorerController,
    row_id: Memo<Option<NodeId>>,
    is_cursor: Memo<bool>,
) -> ElementRef {
    let row_ref = element_ref();
    let active_signal = ctrl.active_row;
    let claimed = Rc::new(Cell::new(None::<NodeId>));
    effect(move || {
        let cur = row_id.get();
        let active = is_cursor.get();
        untracked(|| match (cur, active) {
            (Some(id), true) => {
                claimed.set(Some(id));
                active_signal.set(Some((id, row_ref)));
            }
            _ => {
                if let Some(was) = claimed.replace(None)
                    && active_signal.with(|a| a.is_some_and(|(aid, _)| aid == was))
                {
                    active_signal.set(None);
                }
            }
        });
    });
    row_ref
}

fn make_row_press_handler(
    ctrl: &ExplorerController,
    row_id: Memo<Option<NodeId>>,
) -> impl Fn(&PressEvent) + Clone + 'static {
    let range_ctrl = ctrl.clone();
    let listbox = ctrl.listbox_ref;
    let tree = Arc::clone(&ctrl.tree);

    move |ev: &PressEvent| {
        let Some(id) = row_id.get_untracked() else {
            return;
        };
        if state::exiting_signal().with_untracked(|e| e.nodes.contains(&id)) {
            return;
        }
        listbox.focus();

        if ev.modifiers.shift {
            range_ctrl.select_range_to(id);
            return;
        }
        if ev.modifiers.cmd || ev.modifiers.ctrl {
            state::selection_signal().update(|sel| sel.toggle_item(id));
            return;
        }

        let is_dir = tree
            .node(id)
            .is_some_and(|n| n.kind() == EntryKind::Directory);
        let was_single_cursor = state::selection_signal()
            .with_untracked(|sel| sel.cursor == Some(id) && sel.selected.len() == 1);
        if is_dir && (was_single_cursor || ev.clicks >= 2) {
            state::open_node(id);
        } else {
            state::selection_signal().update(|sel| sel.select_single(id));
        }
    }
}

fn explorer_row_cell(ctrl: &ExplorerController, at: Position) -> View {
    let display = ctrl.data.display_items;
    let row_id = computed(move || display.with(|d| d.get(at.index()).copied()));
    let is_exiting = computed(move || {
        row_id
            .get()
            .is_some_and(|id| state::exiting_signal().with(|e| e.nodes.contains(&id)))
    });
    let is_cursor = computed(move || {
        !is_exiting.get()
            && row_id
                .get()
                .is_some_and(|id| state::selection_signal().with(|s| s.cursor == Some(id)))
    });
    let is_selected = computed(move || {
        !is_exiting.get()
            && row_id
                .get()
                .is_some_and(|id| state::selection_signal().with(|s| s.selected.contains(&id)))
    });
    let multi_selected = ctrl.multi_selected;
    let row_motion = bind_row_motion(row_id, is_exiting);
    let row_ref = bind_active_row(ctrl, row_id, is_cursor);
    let on_press = make_row_press_handler(ctrl, row_id);

    let label_tree = Arc::clone(&ctrl.tree);
    let row_label = move || {
        row_id
            .get()
            .and_then(|id| {
                label_tree
                    .node(id)
                    .map(|n| format!("{}, {}", n.name(), format_bytes(n.total_bytes())))
            })
            .unwrap_or_default()
    };

    let main_line = view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .child(row_left_group(
            ctrl,
            row_id,
            is_cursor,
            is_selected,
            is_exiting,
            on_press.clone(),
        ))
        .child(row_right_group(ctrl, row_id, is_selected, on_press));

    view()
        .role(AccessibilityRole::Option)
        .focusable(false)
        .label(row_label)
        .accessibility_state(move || AccessibilityState {
            selected: is_selected.get(),
            ..AccessibilityState::default()
        })
        .element_ref(&row_ref)
        .group()
        .bind_motion(&row_motion)
        .px_2()
        .py(px(6.0))
        .rounded_md()
        .border_l_1()
        .border_color(move || {
            if is_selected.get() && multi_selected.get() {
                palette::WARN_AMBER
            } else if is_cursor.get() {
                palette::BORDER_ACTIVE
            } else {
                rgba(0)
            }
        })
        .bg(move || {
            if is_exiting.get() {
                palette::WARN_SURFACE
            } else if is_selected.get() {
                palette::BG_SELECTED
            } else {
                palette::BG_CANVAS
            }
        })
        .hover(|s| s.bg(palette::BG_HOVER))
        .child(main_line)
        .child(row_subline(ctrl, row_id, is_cursor))
}

fn row_left_group(
    ctrl: &ExplorerController,
    row_id: Memo<Option<NodeId>>,
    is_cursor: Memo<bool>,
    is_selected: Memo<bool>,
    is_exiting: Memo<bool>,
    on_press: impl Fn(&PressEvent) + Clone + 'static,
) -> View {
    let name_tree = Arc::clone(&ctrl.tree);
    let kind_tree = Arc::clone(&ctrl.tree);
    let hint_tree = Arc::clone(&ctrl.tree);
    let snag_tree = Arc::clone(&ctrl.tree);
    let display = ctrl.data.display_items;
    let spacer_press = on_press.clone();

    let name_hit_area = view()
        .flex_row()
        .items_center()
        .flex_shrink(1.0)
        .min_w(px(0.0))
        .gap_2()
        .cursor(Cursor::Pointer)
        .on_press(on_press)
        .child(
            text(move || {
                if is_cursor.get() {
                    ">"
                } else if is_selected.get() {
                    "✓"
                } else {
                    " "
                }
            })
            .font_family(palette::mono_font())
            .text_size(px(12.0))
            .text_color(move || {
                if !is_cursor.get() && is_selected.get() {
                    palette::WARN_AMBER
                } else {
                    palette::TEXT_PRIMARY
                }
            }),
        )
        .child(
            text(move || {
                let is_dir = row_id
                    .get()
                    .and_then(|id| kind_tree.node(id))
                    .is_some_and(|n| n.kind() == EntryKind::Directory);
                if is_dir { "▸" } else { "•" }
            })
            .font_family(palette::mono_font())
            .text_size(px(11.0))
            .text_color(move || {
                if is_selected.get() {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_FAINT
                }
            }),
        )
        .child(
            text(move || {
                row_id
                    .get()
                    .and_then(|id| name_tree.node(id))
                    .map(|node| {
                        if node.kind() == EntryKind::Directory {
                            format!("{}/", node.name())
                        } else {
                            node.name().to_owned()
                        }
                    })
                    .unwrap_or_default()
            })
            .font_family(palette::mono_font())
            .font_weight(FontWeight::MEDIUM)
            .text_size(px(13.0))
            .text_color(move || {
                if is_selected.get() {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }
            })
            .flex_shrink(1.0)
            .min_w(px(0.0))
            .truncate(),
        );

    let snag_pill = hover_snag_button(snag_tree, row_id, display, is_exiting);
    let trailing_badges = row_trailing_badges(hint_tree, row_id, is_exiting);

    view()
        .flex_row()
        .items_center()
        .flex_1()
        .min_w(px(0.0))
        .gap_2()
        .child(name_hit_area)
        .child(snag_pill)
        .child(trailing_badges)
        .child(
            view()
                .flex_1()
                .h_full()
                .cursor(Cursor::Pointer)
                .on_press(spacer_press),
        )
}

fn row_trailing_badges(
    hint_tree: Arc<StorageTree>,
    row_id: Memo<Option<NodeId>>,
    is_exiting: Memo<bool>,
) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .flex_shrink_0()
        .child(when(
            move || {
                row_id
                    .get()
                    .and_then(|id| hint_tree.node(id))
                    .is_some_and(|n| n.junk_kind().is_some())
            },
            || {
                text("unused")
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::WARN_AMBER)
            },
        ))
        .child(when(
            move || is_exiting.get(),
            || {
                text("→ queued")
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::WARN_AMBER)
            },
        ))
}

fn hover_snag_button(
    tree: Arc<StorageTree>,
    row_id: Memo<Option<NodeId>>,
    display: Memo<Arc<Vec<NodeId>>>,
    is_exiting: Memo<bool>,
) -> View {
    view()
        .role(AccessibilityRole::Button)
        .focusable(false)
        .label(move || {
            let name = row_id
                .get()
                .and_then(|id| tree.node(id).map(NodeRef::name))
                .unwrap_or("item");
            format!("Snag {name}")
        })
        .on_press(move || {
            if is_exiting.get_untracked() {
                return;
            }
            if let Some(id) = row_id.get_untracked() {
                display.with_untracked(|d| state::snag_nodes_in_display(&[id], d));
            }
        })
        .flex_shrink_0()
        .px(px(6.0))
        .py(px(1.5))
        .rounded_sm()
        .bg(palette::WARN_SURFACE)
        .border_1()
        .border_color(palette::WARN_AMBER.opacity(0.6))
        .cursor(Cursor::Pointer)
        .opacity(0.0)
        .pointer_events(PointerEvents::None)
        .group_hover(|s| s.opacity(1.0).pointer_events(PointerEvents::Auto))
        .hover(|s| {
            s.bg(palette::WARN_AMBER.opacity(0.25))
                .border_color(palette::WARN_AMBER)
        })
        .child(
            text("snag")
                .font_family(palette::mono_font())
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(11.0))
                .text_color(palette::WARN_AMBER),
        )
}

fn row_right_group(
    ctrl: &ExplorerController,
    row_id: Memo<Option<NodeId>>,
    is_selected: Memo<bool>,
    on_press: impl Fn(&PressEvent) + 'static,
) -> View {
    let bytes_tree = Arc::clone(&ctrl.tree);
    let pct_tree = Arc::clone(&ctrl.tree);
    let age_label_tree = Arc::clone(&ctrl.tree);
    let age_color_tree = Arc::clone(&ctrl.tree);
    let dot_tree = Arc::clone(&ctrl.tree);
    let scope_bytes = ctrl.scope_bytes;

    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap_3()
        .cursor(Cursor::Pointer)
        .on_press(on_press)
        .child(
            text(move || {
                let bytes = row_id
                    .get()
                    .and_then(|id| bytes_tree.node(id).map(NodeRef::total_bytes))
                    .unwrap_or(0);
                format_bytes(bytes)
            })
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(12.0))
            .text_color(move || {
                if is_selected.get() {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }
            }),
        )
        .child(reactive_mini_dot_bar(dot_tree, row_id, scope_bytes, 10))
        .child(
            text(move || {
                let bytes = row_id
                    .get()
                    .and_then(|id| pct_tree.node(id).map(NodeRef::total_bytes))
                    .unwrap_or(0);
                let total = scope_bytes.get().max(1);
                let pct = (bytes.saturating_mul(100) / total).min(100);
                format!("{pct:>3}%")
            })
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(12.0))
            .text_color(palette::TEXT_FAINT),
        )
        .child(
            text(move || {
                row_id
                    .get()
                    .and_then(|id| age_label_tree.node(id))
                    .map(|node| format_age(node.modified(), age_label_tree.scanned_at()).label)
                    .unwrap_or_default()
            })
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(12.0))
            .text_color(move || {
                let freshness = row_id
                    .get()
                    .and_then(|id| age_color_tree.node(id))
                    .map_or(AgeFreshness::Unknown, |node| {
                        format_age(node.modified(), age_color_tree.scanned_at()).freshness
                    });
                match freshness {
                    AgeFreshness::Stale => palette::WARN_AMBER,
                    AgeFreshness::Today | AgeFreshness::Recent | AgeFreshness::Unknown | _ => {
                        palette::TEXT_MUTED
                    }
                }
            })
            .w(px(46.0)),
        )
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "reactive dot meter maps clamped [0.0, 1.0] share fraction to integer dot count"
)]
fn reactive_mini_dot_bar(
    tree: Arc<StorageTree>,
    row_id: Memo<Option<NodeId>>,
    scope_bytes: Memo<u64>,
    total_dots: usize,
) -> View {
    let meter = computed(move || {
        let Some(node) = row_id.get().and_then(|id| tree.node(id)) else {
            return (0_usize, palette::DOT_EMPTY);
        };
        let total = scope_bytes.get().max(1);
        let share = (node.total_bytes() as f32 / total as f32).clamp(0.0, 1.0);
        let filled = if share <= 0.005 {
            0
        } else {
            ((share * total_dots as f32).round() as usize).clamp(1, total_dots)
        };
        (filled, palette::category_color(node.category()))
    });

    view()
        .flex_row()
        .items_center()
        .gap(px(2.5))
        .children((0..total_dots).map(move |idx| {
            view().w(px(3.0)).h(px(3.0)).rounded_full().bg(move || {
                let (filled, accent) = meter.get();
                if idx < filled {
                    accent
                } else {
                    palette::DOT_EMPTY
                }
            })
        }))
}

fn search_parent_hint(
    tree: &StorageTree,
    node: NodeRef<'_>,
    scope_node: Option<NodeId>,
) -> Option<String> {
    let rel = node.relative_path();
    let parent_rel = rel.parent()?;
    if parent_rel.as_os_str().is_empty() {
        return None;
    }
    let scope_rel = scope_node
        .map(|id| tree.relative_path(id))
        .unwrap_or_default();
    if parent_rel == scope_rel.as_path() {
        return None;
    }
    let shown = if scope_rel.as_os_str().is_empty() {
        parent_rel
    } else {
        parent_rel.strip_prefix(&scope_rel).unwrap_or(parent_rel)
    };
    if shown.as_os_str().is_empty() {
        return None;
    }
    Some(format!("in {}/", shown.display()))
}

fn row_subline(
    ctrl: &ExplorerController,
    row_id: Memo<Option<NodeId>>,
    is_cursor: Memo<bool>,
) -> View {
    let is_searching = ctrl.is_searching;
    let scope_node = ctrl.scope_node;
    let multi_selected = ctrl.multi_selected;
    let show_actions = computed(move || is_cursor.get() && !multi_selected.get());

    let hint_tree = Arc::clone(&ctrl.tree);
    let hint_text = computed(move || {
        if !is_searching.get() {
            return None;
        }
        let id = row_id.get()?;
        let node = hint_tree.node(id)?;
        search_parent_hint(&hint_tree, node, scope_node.get())
    });

    let culprit_tree = Arc::clone(&ctrl.tree);
    let culprit_text = computed(move || {
        let id = row_id.get()?;
        let node = culprit_tree.node(id)?;
        node.largest_culprit().map(|culprit| {
            format!(
                "└ mostly in {}  {} · {}%",
                culprit.relative_subpath,
                format_bytes(culprit.bytes),
                culprit.share_percent
            )
        })
    });

    let meta_tree = Arc::clone(&ctrl.tree);
    let meta_text = computed(move || {
        let id = row_id.get()?;
        let node = meta_tree.node(id)?;
        if node.kind() != EntryKind::Directory {
            return None;
        }
        let item_total = node.file_count() + node.dir_count();
        let age = format_age(node.modified(), meta_tree.scanned_at());
        Some(format!("{item_total} items · changed {}", age.label))
    });

    let actions_tree = Arc::clone(&ctrl.tree);
    let display = ctrl.data.display_items;

    view()
        .pl_6()
        .gap_1()
        .child(when(
            move || hint_text.with(Option::is_some),
            move || {
                text(move || hint_text.get().unwrap_or_default())
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::TEXT_FAINT)
                    .truncate()
            },
        ))
        .child(when(
            move || show_actions.get() && meta_text.with(Option::is_some),
            move || {
                text(move || meta_text.get().unwrap_or_default())
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::TEXT_MUTED)
            },
        ))
        .child(when(
            move || culprit_text.with(Option::is_some),
            move || {
                text(move || culprit_text.get().unwrap_or_default())
                    .font_family(palette::mono_font())
                    .tabular_nums()
                    .text_size(px(11.0))
                    .text_color(move || {
                        if show_actions.get() {
                            palette::TEXT_MUTED
                        } else {
                            palette::TEXT_FAINT
                        }
                    })
                    .truncate()
            },
        ))
        .child(when(
            move || show_actions.get(),
            move || selected_actions_bar(Arc::clone(&actions_tree), row_id, display),
        ))
}

fn selected_actions_bar(
    tree: Arc<StorageTree>,
    row_id: Memo<Option<NodeId>>,
    display: Memo<Arc<Vec<NodeId>>>,
) -> View {
    let has_culprit = computed(move || {
        row_id
            .get()
            .and_then(|id| tree.node(id))
            .is_some_and(|n| n.kind() == EntryKind::Directory && n.largest_culprit().is_some())
    });

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
                if let Some(id) = row_id.get_untracked() {
                    state::open_node(id);
                }
            },
        ))
        .child(when(
            move || has_culprit.get(),
            move || {
                inline_action("g go to culprit", palette::TEXT_MUTED, move || {
                    if let Some(id) = row_id.get_untracked() {
                        state::jump_to_culprit(id);
                    }
                })
            },
        ))
        .child(inline_action(
            "o reveal in finder",
            palette::TEXT_MUTED,
            move || {
                if let Some(id) = row_id.get_untracked() {
                    state::reveal_node(id);
                }
            },
        ))
        .child(inline_action(
            "space preview",
            palette::TEXT_PRIMARY,
            move || {
                if let Some(id) = row_id.get_untracked() {
                    state::preview_node(id);
                }
            },
        ))
        .child(inline_action("s snag", palette::WARN_AMBER, move || {
            if let Some(id) = row_id.get_untracked() {
                display.with_untracked(|d| state::snag_nodes_in_display(&[id], d));
            }
        }))
        .child(inline_action(
            "⌫ move to trash",
            palette::DANGER_RED,
            move || {
                if let Some(id) = row_id.get_untracked() {
                    state::quick_trash_node(id);
                }
            },
        ))
}

fn inline_action(
    label: &'static str,
    color: nsh_core::prelude::Rgba,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
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
        .min_h(px(0.0))
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
        .min_h(px(0.0))
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

fn agg_tab(label: &'static str, active: bool, kind: AggregateKind) -> ButtonElement {
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
        let covered = state::snag_signal().with(|s| s.queue.is_node_covered(tree, id));
        if covered {
            return None;
        }
        let accent = palette::category_color(node.category());
        let rel = node.relative_path().display().to_string();

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
                            text(format_bytes(node.total_bytes()))
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
