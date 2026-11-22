//! Settings modal for inspecting, rebinding, and resetting keyboard & extra-mouse-button shortcuts.

use nsh_base::button;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::px;
use nsh_core::prelude::rgba;
use nsh_core::prelude::scroll;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::keybindings::Action;
use crate::keybindings::ActionSection;
use crate::keybindings::InputBinding;
use crate::keybindings::KeyBindings;
use crate::palette;
use crate::state;
use crate::state::RecordMode;

/// Renders the keyboard & mouse shortcut settings overlay when open.
#[expect(
    clippy::too_many_lines,
    reason = "settings modal composes header, live key/mouse recorder banner, and grouped action rows"
)]
pub(crate) fn settings_modal() -> Option<View> {
    let settings = state::settings_signal().get();
    if !settings.open {
        return None;
    }

    let bindings = state::keybindings_signal().get();
    let recording = settings.recording;

    let capture_banner = recording.map(|(action, mode)| {
        let verb = match mode {
            RecordMode::Replace => "Rebinding",
            RecordMode::Add => "Adding binding for",
        };

        view()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(palette::WARN_SURFACE)
            .border_1()
            .border_color(palette::WARN_AMBER)
            .child(
                view()
                    .gap(px(2.0))
                    .flex_1()
                    .child(
                        text(format!("{verb} `{}`...", action.label()))
                            .font_family(palette::mono_font())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(12.0))
                            .text_color(palette::WARN_AMBER),
                    )
                    .child(
                        text("Press any key combination or extra mouse button (Back / Forward / Middle) · Esc cancels")
                            .font_family(palette::mono_font())
                            .text_size(px(10.0))
                            .text_color(palette::TEXT_MUTED),
                    ),
            )
            .child(
                button()
                    .accessibility_label("Cancel key recording")
                    .on_press(state::cancel_recording_binding)
                    .px_2()
                    .py(px(3.0))
                    .rounded_sm()
                    .bg(palette::BG_INSET)
                    .border_1()
                    .border_color(palette::BORDER_SUBTLE)
                    .cursor(Cursor::Pointer)
                    .child(
                        text("Cancel")
                            .font_family(palette::mono_font())
                            .text_size(px(11.0))
                            .text_color(palette::TEXT_MUTED),
                    ),
            )
    });

    let sections = view()
        .gap_4()
        .children(ActionSection::ALL.into_iter().map(|section| {
            let rows = Action::ALL
                .into_iter()
                .filter(|action| action.section() == section)
                .map(|action| action_row(action, &bindings, recording));

            view()
                .gap_1()
                .child(
                    text(section.title())
                        .font_family(palette::mono_font())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(11.0))
                        .text_color(palette::TEXT_FAINT)
                        .pb_1(),
                )
                .children(rows)
        }));

    Some(
        view()
            .absolute()
            .inset_0()
            .z_index(110)
            .bg(rgba(0x00_00_00_cc))
            .items_center()
            .justify_center()
            .p_6()
            .child(
                view()
                    .w(px(640.0))
                    .max_h(px(620.0))
                    .p_5()
                    .rounded_lg()
                    .bg(palette::BG_SURFACE)
                    .border_1()
                    .border_color(palette::BORDER_ACTIVE)
                    .gap_4()
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                view()
                                    .gap(px(2.0))
                                    .child(
                                        text("Keyboard & Mouse Shortcuts")
                                            .font_family(palette::mono_font())
                                            .font_weight(FontWeight::BOLD)
                                            .text_size(px(15.0))
                                            .text_color(palette::TEXT_PRIMARY),
                                    )
                                    .child(
                                        text("Click Rebind or + Add, then press a key combo or extra mouse button")
                                            .font_family(palette::mono_font())
                                            .text_size(px(11.0))
                                            .text_color(palette::TEXT_FAINT),
                                    ),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        button()
                                            .accessibility_label("Reset all shortcuts to defaults")
                                            .on_press(state::reset_all_keybindings)
                                            .px_2()
                                            .py(px(4.0))
                                            .rounded_sm()
                                            .bg(palette::BG_INSET)
                                            .border_1()
                                            .border_color(palette::BORDER_SUBTLE)
                                            .cursor(Cursor::Pointer)
                                            .child(
                                                text("Reset defaults")
                                                    .font_family(palette::mono_font())
                                                    .text_size(px(11.0))
                                                    .text_color(palette::TEXT_MUTED),
                                            ),
                                    )
                                    .child(
                                        button()
                                            .accessibility_label("Close settings")
                                            .on_press(state::close_settings)
                                            .px_2()
                                            .py(px(4.0))
                                            .rounded_sm()
                                            .bg(palette::BG_INSET)
                                            .border_1()
                                            .border_color(palette::BORDER_SUBTLE)
                                            .cursor(Cursor::Pointer)
                                            .child(
                                                text("✕")
                                                    .font_family(palette::mono_font())
                                                    .text_size(px(12.0))
                                                    .text_color(palette::TEXT_MUTED),
                                            ),
                                    ),
                            ),
                    )
                    .child(capture_banner)
                    .child(
                        scroll()
                            .flex_1()
                            .max_h(px(440.0))
                            .pr_2()
                            .child(sections),
                    )
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .pt_1()
                            .border_t_1()
                            .border_color(palette::BORDER_SUBTLE)
                            .child(
                                text("Mouse Back / Forward / Middle buttons work anywhere in the window")
                                    .font_family(palette::mono_font())
                                    .text_size(px(10.0))
                                    .text_color(palette::TEXT_FAINT),
                            )
                            .child(
                                button()
                                    .accessibility_label("Done editing settings")
                                    .on_press(state::close_settings)
                                    .px_3()
                                    .py(px(5.0))
                                    .rounded_md()
                                    .bg(palette::BG_SELECTED)
                                    .border_1()
                                    .border_color(palette::BORDER_ACTIVE)
                                    .cursor(Cursor::Pointer)
                                    .child(
                                        text("Done")
                                            .font_family(palette::mono_font())
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_size(px(11.0))
                                            .text_color(palette::TEXT_PRIMARY),
                                    ),
                            ),
                    ),
            ),
    )
}

fn action_row(
    action: Action,
    bindings: &KeyBindings,
    recording: Option<(Action, RecordMode)>,
) -> View {
    let is_recording_this = recording.is_some_and(|(rec_action, _)| rec_action == action);
    let current = bindings.bindings_for(action);
    let is_default = current == action.default_bindings().as_slice();

    let chips = if current.is_empty() {
        view().flex_row().items_center().child(
            text("unbound")
                .font_family(palette::mono_font())
                .text_size(px(10.0))
                .text_color(palette::TEXT_FAINT),
        )
    } else {
        view()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children(
                current
                    .iter()
                    .enumerate()
                    .map(|(idx, binding)| binding_chip(action, idx, binding)),
            )
    };

    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py(px(6.0))
        .rounded_md()
        .bg(if is_recording_this {
            palette::WARN_SURFACE
        } else {
            palette::BG_INSET
        })
        .border_1()
        .border_color(if is_recording_this {
            palette::WARN_AMBER
        } else {
            palette::BORDER_SUBTLE
        })
        .child(
            text(action.label())
                .font_family(palette::mono_font())
                .text_size(px(12.0))
                .text_color(palette::TEXT_PRIMARY)
                .flex_shrink_0(),
        )
        .child(action_controls(action, chips, is_default))
}

fn action_controls(action: Action, chips: View, is_default: bool) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(chips)
        .child(
            button()
                .accessibility_label(format!("Rebind {}", action.label()))
                .on_press(move || {
                    state::start_recording_binding(action, RecordMode::Replace);
                })
                .px_2()
                .py(px(2.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .cursor(Cursor::Pointer)
                .child(
                    text("Rebind")
                        .font_family(palette::mono_font())
                        .text_size(px(10.0))
                        .text_color(palette::TEXT_MUTED),
                ),
        )
        .child(
            button()
                .accessibility_label(format!("Add binding for {}", action.label()))
                .on_press(move || {
                    state::start_recording_binding(action, RecordMode::Add);
                })
                .px(px(5.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .cursor(Cursor::Pointer)
                .child(
                    text("+")
                        .font_family(palette::mono_font())
                        .text_size(px(10.0))
                        .text_color(palette::TEXT_MUTED),
                ),
        )
        .child((!is_default).then(|| {
            button()
                .accessibility_label(format!("Reset {}", action.label()))
                .on_press(move || state::reset_action_binding(action))
                .px(px(5.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(palette::BG_SURFACE)
                .border_1()
                .border_color(palette::BORDER_SUBTLE)
                .cursor(Cursor::Pointer)
                .child(
                    text("↺")
                        .font_family(palette::mono_font())
                        .text_size(px(10.0))
                        .text_color(palette::TEXT_FAINT),
                )
        }))
}

fn binding_chip(action: Action, idx: usize, binding: &InputBinding) -> View {
    let label = binding.display_label();
    view()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .px(px(6.0))
        .py(px(2.0))
        .rounded_sm()
        .bg(palette::BG_SELECTED)
        .border_1()
        .border_color(palette::BORDER_ACTIVE)
        .child(
            text(label.clone())
                .font_family(palette::mono_font())
                .text_size(px(10.0))
                .text_color(palette::TEXT_PRIMARY),
        )
        .child(
            button()
                .accessibility_label(format!("Remove {label} from {}", action.label()))
                .on_press(move || state::remove_action_binding(action, idx))
                .cursor(Cursor::Pointer)
                .child(
                    text("×")
                        .font_family(palette::mono_font())
                        .text_size(px(10.0))
                        .text_color(palette::TEXT_FAINT),
                ),
        )
}
