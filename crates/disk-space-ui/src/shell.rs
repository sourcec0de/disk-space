//! Top-level application shell: header bar, capacity headline, dot map, workspace split, and keys.

use std::sync::Arc;

use disk_space_core::FilterMode;
use disk_space_core::StorageTree;
use disk_space_core::format_bytes;
use nsh_base::button;
use nsh_core::file_dialog::OpenDialog;
use nsh_core::keymap::When;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::KeyOptions;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::dynamic;
use nsh_core::prelude::on_key_with;
use nsh_core::prelude::pick_paths;
use nsh_core::prelude::px;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::dot_matrix;
use crate::explorer;
use crate::palette;
use crate::sidebar;
use crate::snag_drawer;
use crate::state;
use crate::state::NoticeTone;
use crate::state::ScanPhase;
use crate::state::ViewTab;

/// Renders the primary shell and registers keyboard navigation bindings.
pub(crate) fn shell() -> impl IntoElement {
    register_shortcuts();

    view()
        .flex_1()
        .bg(palette::BG_CANVAS)
        .px_6()
        .py_5()
        .gap_4()
        .child(top_header_bar())
        .child(capacity_headline())
        .child(notice_banner())
        .child(dot_matrix::dot_matrix_bar())
        .child(dynamic(|| {
            let phase = state::scan_signal().with(|s| s.phase.clone());
            match phase {
                ScanPhase::Idle => scanning_view(0, 0, 0, "Preparing scan root..."),
                ScanPhase::Scanning(prog) => scanning_view(
                    prog.scanned_files,
                    prog.scanned_dirs,
                    prog.scanned_bytes,
                    &prog.current_path,
                ),
                ScanPhase::Failed(err) => failed_view(&err),
                ScanPhase::Ready(tree) => ready_workspace(&tree),
            }
        }))
        .child(dynamic(snag_drawer::confirm_modal))
}

fn ready_workspace(tree: &Arc<StorageTree>) -> View {
    view()
        .flex_1()
        .flex_row()
        .gap_4()
        .child(sidebar::categories_sidebar(tree))
        .child(explorer::explorer_pane(tree))
        .child(snag_drawer::snag_drawer(tree))
}

fn scanning_view(files: u64, dirs: u64, bytes: u64, path: &str) -> View {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            text(format!(
                "Scanning inside root jail · {} · {files} files · {dirs} folders",
                format_bytes(bytes)
            ))
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(13.0))
            .text_color(palette::TEXT_PRIMARY),
        )
        .child(
            text(path.to_owned())
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(palette::TEXT_FAINT)
                .max_w(px(640.0))
                .truncate(),
        )
}

fn failed_view(error: &str) -> View {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_3()
        .child(
            text(format!("Scan blocked or failed: {error}"))
                .font_family(palette::mono_font())
                .text_size(px(13.0))
                .text_color(palette::DANGER_RED),
        )
        .child(
            button()
                .accessibility_label("Choose another folder")
                .on_press(open_root_picker)
                .px_3()
                .py_1()
                .rounded_md()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_ACTIVE)
                .cursor(Cursor::Pointer)
                .child(
                    text("Choose Scan Root")
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_PRIMARY),
                ),
        )
}

fn top_header_bar() -> impl IntoElement {
    view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_4()
        .child(header_left_group())
        .child(header_right_group())
}

fn header_left_group() -> View {
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .child(
            text("disk-space")
                .font_family(palette::mono_font())
                .font_weight(FontWeight::BOLD)
                .text_size(px(13.0))
                .text_color(palette::TEXT_PRIMARY),
        )
        .child(
            text(move || {
                state::current_tree().map_or_else(
                    || "scanning...".to_owned(),
                    |tree| tree.volume().name.clone(),
                )
            })
            .font_family(palette::mono_font())
            .text_size(px(13.0))
            .text_color(palette::TEXT_MUTED),
        )
        .child(
            button()
                .accessibility_label("Change scan root")
                .on_press(open_root_picker)
                .max_w(px(280.0))
                .px_2()
                .py(px(3.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .cursor(Cursor::Pointer)
                .child(
                    text(move || {
                        state::scan_signal().with(|s| {
                            s.root_path.as_ref().map_or_else(
                                || "root: ~".to_owned(),
                                |path| format!("root: {}", state::display_abs_path(path)),
                            )
                        })
                    })
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::TEXT_MUTED)
                    .truncate(),
                ),
        )
        .child(
            button()
                .accessibility_label("Rescan root")
                .on_press(|| {
                    if let Some(root) = state::scan_signal().with_untracked(|s| s.root_path.clone())
                    {
                        state::start_scan(root);
                    }
                })
                .px_2()
                .py(px(3.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .cursor(Cursor::Pointer)
                .child(
                    text("↻ rescan")
                        .font_family(palette::mono_font())
                        .text_size(px(11.0))
                        .text_color(palette::TEXT_MUTED),
                ),
        )
}

fn header_right_group() -> View {
    let permission_hint = dynamic(|| {
        state::current_tree()
            .and_then(|tree| {
                let count = tree.unreadable_dirs();
                let host = state::create_host();
                host.access_hint(count).map(|hint| (count, hint))
            })
            .map(|(count, hint)| {
                button()
                    .accessibility_label("Grant disk access")
                    .on_press(|| {
                        let host = state::create_host();
                        if let Err(err) = host.open_permission_settings() {
                            state::set_notice(NoticeTone::Warn, err.to_string());
                        }
                    })
                    .cursor(Cursor::Pointer)
                    .child(
                        text(format!("{count} folders unreadable · {}", hint.summary))
                            .font_family(palette::mono_font())
                            .text_size(px(12.0))
                            .text_color(palette::TEXT_FAINT),
                    )
            })
    });

    view()
        .flex_row()
        .items_center()
        .gap_3()
        .child(permission_hint)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_1()
                .child(view_tab_btn("Dots", ViewTab::DotMatrix))
                .child(view_tab_btn("Sunburst", ViewTab::Sunburst))
                .child(view_tab_btn("Aggregates", ViewTab::Aggregates)),
        )
        .child(
            button()
                .accessibility_label("Toggle destruction queue drawer")
                .on_press(|| state::snag_signal().update(|s| s.drawer_open = !s.drawer_open))
                .px_2()
                .py(px(3.0))
                .rounded_sm()
                .cursor(Cursor::Pointer)
                .bg(move || {
                    let count = state::snag_signal().with(|s| s.queue.items().len());
                    if count > 0 {
                        palette::DANGER_SURFACE
                    } else {
                        palette::BG_SURFACE
                    }
                })
                .border_1()
                .border_color(move || {
                    let count = state::snag_signal().with(|s| s.queue.items().len());
                    if count > 0 {
                        palette::DANGER_RED
                    } else {
                        palette::BORDER_SUBTLE
                    }
                })
                .child(
                    text(move || {
                        let (count, bytes) = state::snag_signal()
                            .with(|s| (s.queue.items().len(), s.queue.total_bytes()));
                        if count == 0 {
                            "Snagged (0)".to_owned()
                        } else {
                            format!("Snagged ({count} · {})", format_bytes(bytes))
                        }
                    })
                    .font_family(palette::mono_font())
                    .tabular_nums()
                    .text_size(px(11.0))
                    .text_color(move || {
                        let count = state::snag_signal().with(|s| s.queue.items().len());
                        if count > 0 {
                            palette::DANGER_RED
                        } else {
                            palette::TEXT_MUTED
                        }
                    }),
                ),
        )
}

fn view_tab_btn(label: &'static str, tab: ViewTab) -> impl IntoElement {
    button()
        .accessibility_label(format!("View {label}"))
        .on_press(move || state::browse_signal().update(|b| b.view_tab = tab))
        .px_2()
        .py(px(3.0))
        .rounded_sm()
        .cursor(Cursor::Pointer)
        .bg(move || {
            if state::browse_signal().with(|b| b.view_tab == tab) {
                palette::BG_SELECTED
            } else {
                palette::BG_SURFACE
            }
        })
        .border_1()
        .border_color(move || {
            if state::browse_signal().with(|b| b.view_tab == tab) {
                palette::BORDER_ACTIVE
            } else {
                palette::BORDER_SUBTLE
            }
        })
        .child(
            text(label)
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(move || {
                    if state::browse_signal().with(|b| b.view_tab == tab) {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_MUTED
                    }
                }),
        )
}

fn capacity_headline() -> impl IntoElement {
    text(move || {
        let Some(tree) = state::current_tree() else {
            return "Scanning disk usage...".to_owned();
        };
        let vol = tree.volume();
        let root_scanned = tree.node(tree.root_id()).map_or(0, |n| n.total_bytes);
        let staged = state::snag_signal().with(|s| s.queue.total_bytes());

        let base = if vol.total_bytes > 0 {
            let pct = vol.used_bytes.saturating_mul(100) / vol.total_bytes.max(1);
            format!(
                "{} used of {}   ·   {} free   ·   {pct}% full   ·   {} in root",
                format_bytes(vol.used_bytes),
                format_bytes(vol.total_bytes),
                format_bytes(vol.available_bytes),
                format_bytes(root_scanned),
            )
        } else {
            format!("{} scanned inside root", format_bytes(root_scanned))
        };

        if staged > 0 {
            format!("{base}   ·   -{} staged", format_bytes(staged))
        } else {
            base
        }
    })
    .font_family(palette::mono_font())
    .tabular_nums()
    .text_size(px(13.0))
    .text_color(palette::TEXT_PRIMARY)
}

fn notice_banner() -> impl IntoElement {
    dynamic(|| {
        state::notice_signal()
            .with(|n| n.notice.clone())
            .map(|notice| {
                let (bg, fg) = match notice.tone {
                    NoticeTone::Info => (palette::BG_SURFACE, palette::TEXT_PRIMARY),
                    NoticeTone::Warn => (palette::WARN_SURFACE, palette::WARN_AMBER),
                    NoticeTone::Danger => (palette::DANGER_SURFACE, palette::DANGER_RED),
                };
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .rounded_sm()
                    .bg(bg)
                    .border_1()
                    .border_color(palette::BORDER_SUBTLE)
                    .child(
                        text(notice.text)
                            .font_family(palette::mono_font())
                            .text_size(px(11.0))
                            .text_color(fg)
                            .truncate(),
                    )
                    .child(
                        button()
                            .accessibility_label("Dismiss notice")
                            .on_press(state::clear_notice)
                            .cursor(Cursor::Pointer)
                            .child(
                                text("×")
                                    .font_family(palette::mono_font())
                                    .text_size(px(12.0))
                                    .text_color(palette::TEXT_MUTED),
                            ),
                    )
            })
    })
}

fn open_root_picker() {
    state::spawn_app_task(async {
        let dialog = OpenDialog::folders()
            .title("Choose Scan Root")
            .button_label("Scan Folder");
        if let Ok(Some(paths)) = pick_paths(dialog).await
            && let Some(chosen) = paths.into_iter().next()
        {
            state::start_scan(chosen);
        }
    });
}

fn register_shortcuts() {
    let nav_opts = KeyOptions {
        when: When::NotEditing,
        ..KeyOptions::default()
    };

    for key in ["down", "j"] {
        on_key_with(key, nav_opts, |_| move_selection(1));
    }
    for key in ["up", "k"] {
        on_key_with(key, nav_opts, |_| move_selection(-1));
    }
    for key in ["right", "enter"] {
        on_key_with(key, nav_opts, |_| {
            if let Some(id) = selected_node_id() {
                state::open_node(id);
            }
        });
    }
    for key in ["left", "h", "backspace"] {
        on_key_with(key, nav_opts, |_| state::ascend_directory());
    }
    on_key_with("g", nav_opts, |_| {
        if let Some(id) = selected_node_id() {
            state::jump_to_culprit(id);
        }
    });
    on_key_with("o", nav_opts, |_| {
        if let Some(id) = selected_node_id() {
            state::reveal_node(id);
        }
    });
    on_key_with("s", nav_opts, |_| {
        if let Some(id) = selected_node_id() {
            state::toggle_snag_node(id);
        }
    });
    on_key_with("f", nav_opts, |_| {
        state::browse_signal().update(|b| {
            b.filter_mode = match b.filter_mode {
                FilterMode::All => FilterMode::JunkAndUnusedOnly,
                FilterMode::JunkAndUnusedOnly | _ => FilterMode::All,
            };
            b.selected_row = 0;
        });
    });
    on_key_with("d", nav_opts, |_| {
        state::snag_signal().update(|s| s.drawer_open = !s.drawer_open);
    });
    on_key_with("v", nav_opts, |_| {
        state::browse_signal().update(|b| {
            b.view_tab = match b.view_tab {
                ViewTab::DotMatrix => ViewTab::Sunburst,
                ViewTab::Sunburst => ViewTab::Aggregates,
                ViewTab::Aggregates => ViewTab::DotMatrix,
            };
        });
    });
    on_key_with("r", nav_opts, |_| {
        if let Some(root) = state::scan_signal().with_untracked(|s| s.root_path.clone()) {
            state::start_scan(root);
        }
    });

    on_key_with(
        "escape",
        KeyOptions {
            when: When::Always,
            ..KeyOptions::default()
        },
        |_| {
            let confirm_open = state::snag_signal().with_untracked(|s| s.confirm_open);
            if confirm_open {
                state::snag_signal().update(|s| {
                    s.confirm_open = false;
                    s.erase_input.clear();
                });
                return;
            }
            let has_search = state::browse_signal().with_untracked(|b| !b.search_text.is_empty());
            if has_search {
                state::browse_signal().update(|b| {
                    b.search_text.clear();
                    b.selected_row = 0;
                });
                return;
            }
            state::ascend_directory();
        },
    );
}

fn move_selection(delta: isize) {
    let Some(tree) = state::current_tree_untracked() else {
        return;
    };
    let count = state::visible_items(&tree).len();
    if count == 0 {
        return;
    }
    state::browse_signal().update(|b| {
        let cur = b.selected_row.min(count - 1);
        if delta < 0 {
            b.selected_row = cur.saturating_sub(delta.unsigned_abs());
        } else {
            b.selected_row = cur.saturating_add(delta.cast_unsigned()).min(count - 1);
        }
    });
}

fn selected_node_id() -> Option<disk_space_core::NodeId> {
    let tree = state::current_tree_untracked()?;
    let items = state::visible_items(&tree);
    if items.is_empty() {
        return None;
    }
    let idx = state::browse_signal().with_untracked(|b| b.selected_row.min(items.len() - 1));
    items.get(idx).copied()
}
