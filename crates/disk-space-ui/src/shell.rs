//! Top-level application shell: header bar, capacity headline, dot map, workspace split, and keys.

use std::sync::Arc;

use disk_space_core::FilterMode;
use disk_space_core::NodeRef;
use disk_space_core::StorageTree;
use disk_space_core::format_bytes;
use nsh_base::button;
use nsh_core::file_dialog::OpenDialog;
use nsh_core::keymap::When;
use nsh_core::prelude::ComponentBuilder;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::Element;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::Interactive;
use nsh_core::prelude::IntoElement;
use nsh_core::prelude::KeyCapture;
use nsh_core::prelude::KeyEvent;
use nsh_core::prelude::KeyOptions;
use nsh_core::prelude::MouseButton;
use nsh_core::prelude::MouseEvent;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::batch;
use nsh_core::prelude::computed;
use nsh_core::prelude::dynamic;
use nsh_core::prelude::effect;
use nsh_core::prelude::element_ref;
use nsh_core::prelude::on_key_with;
use nsh_core::prelude::pick_paths;
use nsh_core::prelude::px;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::dot_matrix;
use crate::explorer;
use crate::keybindings::Action;
use crate::keybindings::InputBinding;
use crate::keybindings::key_event_to_binding;
use crate::palette;
use crate::settings_modal;
use crate::sidebar;
use crate::snag_drawer;
use crate::state;
use crate::state::NoticeTone;
use crate::state::ScanPhase;
use crate::state::ViewTab;

/// Renders the primary shell and registers keyboard navigation bindings.
pub(crate) fn shell() -> impl IntoElement {
    register_shortcuts();

    let recorder_ref = element_ref();
    effect(move || {
        if state::settings_signal().with(|s| s.recording.is_some()) {
            recorder_ref.focus();
        } else {
            recorder_ref.blur();
        }
    });

    view()
        .flex_1()
        .bg(palette::BG_CANVAS)
        .on_mouse_up(handle_global_mouse_up)
        .px_6()
        .py_5()
        .gap_4()
        .child(
            view()
                .element_ref(&recorder_ref)
                .focusable(true)
                .capture_keys(KeyCapture::All)
                .on_key_down(handle_recording_key_down)
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .w(px(1.0))
                .h(px(1.0)),
        )
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
                ScanPhase::Ready(ref tree) => ready_workspace(tree),
            }
        }))
        .child(dynamic(settings_modal::settings_modal))
        .child(dynamic(snag_drawer::confirm_modal))
}

fn ready_workspace(tree: &Arc<StorageTree>) -> View {
    let sidebar_tree = Arc::clone(tree);
    let drawer_tree = Arc::clone(tree);
    let selected_cat = computed(|| state::browse_signal().with(|b| b.selected_category));
    view()
        .flex_1()
        .min_h(px(0.0))
        .flex_row()
        .gap_4()
        .child(dynamic(move || {
            sidebar::categories_sidebar(&sidebar_tree, selected_cat.get())
        }))
        .child(explorer::explorer_pane(tree))
        .child(dynamic(move || snag_drawer::snag_drawer(&drawer_tree)))
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
        .child(nav_history_buttons())
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
        .child(settings_toggle_btn())
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

fn settings_toggle_btn() -> impl IntoElement {
    button()
        .accessibility_label("Keyboard and mouse settings")
        .on_press(state::toggle_settings)
        .px_2()
        .py(px(3.0))
        .rounded_sm()
        .cursor(Cursor::Pointer)
        .bg(move || {
            if state::settings_signal().with(|s| s.open) {
                palette::BG_SELECTED
            } else {
                palette::BG_SURFACE
            }
        })
        .border_1()
        .border_color(move || {
            if state::settings_signal().with(|s| s.open) {
                palette::BORDER_ACTIVE
            } else {
                palette::BORDER_SUBTLE
            }
        })
        .child(
            text("Keys")
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(move || {
                    if state::settings_signal().with(|s| s.open) {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_MUTED
                    }
                }),
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
        let root_scanned = tree.node(tree.root_id()).map_or(0, NodeRef::total_bytes);
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

fn nav_history_buttons() -> View {
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .child(
            button()
                .accessibility_label("Navigate back")
                .disabled(move || state::nav_history_signal().with(|h| h.back.is_empty()))
                .on_press(state::navigate_back)
                .px(px(6.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .disabled_style(|s| s.opacity(0.35))
                .cursor(Cursor::Pointer)
                .child(
                    text("‹")
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_PRIMARY),
                ),
        )
        .child(
            button()
                .accessibility_label("Navigate forward")
                .disabled(move || state::nav_history_signal().with(|h| h.forward.is_empty()))
                .on_press(state::navigate_forward)
                .px(px(6.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .disabled_style(|s| s.opacity(0.35))
                .cursor(Cursor::Pointer)
                .child(
                    text("›")
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_PRIMARY),
                ),
        )
}

fn toggle_junk_filter() {
    batch(|| {
        state::browse_signal().update(|b| {
            b.filter_mode = match b.filter_mode {
                FilterMode::All => FilterMode::JunkAndUnusedOnly,
                FilterMode::JunkAndUnusedOnly | _ => FilterMode::All,
            };
        });
        state::selection_signal().update(state::SelectionState::clear);
    });
}

fn cycle_view_tab() {
    state::browse_signal().update(|b| {
        b.view_tab = match b.view_tab {
            ViewTab::DotMatrix => ViewTab::Sunburst,
            ViewTab::Sunburst => ViewTab::Aggregates,
            ViewTab::Aggregates => ViewTab::DotMatrix,
        };
    });
}

fn rescan_active_root() {
    if let Some(root) = state::scan_signal().with_untracked(|s| s.root_path.clone()) {
        state::start_scan(root);
    }
}

fn handle_global_mouse_up(ev: &MouseEvent) {
    let Some(button) = ev.button else {
        return;
    };
    if !matches!(
        button,
        MouseButton::Back | MouseButton::Forward | MouseButton::Middle
    ) {
        return;
    }

    if state::settings_signal().with_untracked(|s| s.recording.is_some()) {
        state::apply_recorded_binding(InputBinding::mouse(button, ev.modifiers));
        return;
    }

    if !state::can_dispatch_app_shortcut() {
        return;
    }

    let action =
        state::keybindings_signal().with_untracked(|kb| kb.action_for_mouse(button, ev.modifiers));
    if let Some(action) = action {
        dispatch_mouse_action(action);
    }
}

fn dispatch_mouse_action(action: Action) {
    match action {
        Action::NavigateBack => state::navigate_back(),
        Action::NavigateForward => state::navigate_forward(),
        Action::AscendDirectory => state::ascend_directory(),
        Action::ToggleJunkFilter => toggle_junk_filter(),
        Action::ToggleQueueDrawer => {
            state::snag_signal().update(|s| s.drawer_open = !s.drawer_open);
        }
        Action::CycleViewTab => cycle_view_tab(),
        Action::RescanRoot => rescan_active_root(),
        Action::ReviewAndExecute => state::open_destruction_review(),
        Action::OpenSettings => state::toggle_settings(),
        Action::QuickLook => {
            if let Some(id) = state::selection_signal().with_untracked(|sel| sel.cursor) {
                state::preview_node(id);
            }
        }
        Action::RevealInFinder => {
            if let Some(id) = state::selection_signal().with_untracked(|sel| sel.cursor) {
                state::reveal_node(id);
            }
        }
        Action::OpenSelected => {
            if let Some(id) = state::selection_signal().with_untracked(|sel| sel.cursor) {
                state::open_node(id);
            }
        }
        Action::JumpToCulprit => {
            if let Some(id) = state::selection_signal().with_untracked(|sel| sel.cursor) {
                state::jump_to_culprit(id);
            }
        }
        Action::SnagSelected => {
            if let Some(id) = state::selection_signal().with_untracked(|sel| sel.cursor) {
                state::snag_node(id);
            }
        }
        Action::FocusSearch | Action::MoveDown | Action::MoveUp | Action::SelectAll => {}
    }
}

fn shortcut_opts(spec: &str, always_when_modified: bool) -> KeyOptions {
    let has_modifier = spec.contains("mod+")
        || spec.contains("cmd+")
        || spec.contains("ctrl+")
        || spec.contains("alt+");
    KeyOptions {
        when: if always_when_modified && has_modifier {
            When::Always
        } else {
            When::NotEditing
        },
        repeat: false,
        ..KeyOptions::default()
    }
}

fn register_shortcuts() {
    effect(|| {
        let kb = state::keybindings_signal().get();

        for key in kb.key_strings_for(Action::ToggleJunkFilter) {
            on_key_with(key, shortcut_opts(key, false), |_| {
                if state::can_dispatch_app_shortcut() {
                    toggle_junk_filter();
                }
            });
        }
        for key in kb.key_strings_for(Action::ToggleQueueDrawer) {
            on_key_with(key, shortcut_opts(key, false), |_| {
                if state::can_dispatch_app_shortcut() {
                    state::snag_signal().update(|s| s.drawer_open = !s.drawer_open);
                }
            });
        }
        for key in kb.key_strings_for(Action::CycleViewTab) {
            on_key_with(key, shortcut_opts(key, false), |_| {
                if state::can_dispatch_app_shortcut() {
                    cycle_view_tab();
                }
            });
        }
        for key in kb.key_strings_for(Action::RescanRoot) {
            on_key_with(key, shortcut_opts(key, false), |_| {
                if state::can_dispatch_app_shortcut() {
                    rescan_active_root();
                }
            });
        }
        for key in kb.key_strings_for(Action::NavigateBack) {
            on_key_with(key, shortcut_opts(key, true), |_| {
                if state::can_dispatch_app_shortcut() {
                    state::navigate_back();
                }
            });
        }
        for key in kb.key_strings_for(Action::NavigateForward) {
            on_key_with(key, shortcut_opts(key, true), |_| {
                if state::can_dispatch_app_shortcut() {
                    state::navigate_forward();
                }
            });
        }
        for key in kb.key_strings_for(Action::ReviewAndExecute) {
            on_key_with(key, shortcut_opts(key, true), |_| {
                if state::can_dispatch_app_shortcut() {
                    state::open_destruction_review();
                }
            });
        }
        for key in kb.key_strings_for(Action::OpenSettings) {
            on_key_with(key, shortcut_opts(key, true), |_| {
                if !state::snag_signal().with_untracked(|s| s.confirm_open) {
                    state::toggle_settings();
                }
            });
        }
    });

    let confirm_open = computed(|| state::snag_signal().with(|s| s.confirm_open));
    effect(move || {
        if confirm_open.get() {
            on_key_with(
                "enter",
                KeyOptions {
                    when: When::Always,
                    ..KeyOptions::default()
                },
                |_| state::confirm_execute_if_ready(),
            );
        }
    });

    on_key_with(
        "escape",
        KeyOptions {
            when: When::Always,
            ..KeyOptions::default()
        },
        |_| handle_escape(),
    );
}

fn handle_escape() {
    let settings = state::settings_signal().get_untracked();
    if settings.recording.is_some() {
        state::cancel_recording_binding();
        return;
    }
    if settings.open {
        state::close_settings();
        return;
    }
    let confirm_open = state::snag_signal().with_untracked(|s| s.confirm_open);
    if confirm_open {
        state::snag_signal().update(|s| {
            s.confirm_open = false;
            s.erase_input.clear();
        });
        return;
    }
    let has_multi = state::selection_signal().with_untracked(|sel| sel.selected.len() > 1);
    if has_multi {
        state::selection_signal().update(state::SelectionState::collapse_to_cursor);
        return;
    }
    let has_search = state::browse_signal().with_untracked(|b| !b.search_text.is_empty());
    if has_search {
        batch(|| {
            state::browse_signal().update(|b| {
                b.search_text.clear();
            });
            state::selection_signal().update(state::SelectionState::clear);
        });
        return;
    }
    state::ascend_directory();
}

fn handle_recording_key_down(ev: &KeyEvent) {
    if state::settings_signal().with_untracked(|s| s.recording.is_none()) {
        return;
    }
    if ev.key.as_str().eq_ignore_ascii_case("Escape")
        && ev.modifiers == nsh_core::keymap::Modifiers::default()
    {
        state::cancel_recording_binding();
        return;
    }
    if let Some(binding) = key_event_to_binding(ev) {
        state::apply_recorded_binding(binding);
    }
}
