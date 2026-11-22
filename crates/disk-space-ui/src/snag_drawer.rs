//! Collapsible destruction & multi-pass secure erasure drawer and confirmation modal.

use disk_space_core::DestructionMode;
use disk_space_core::ErasePasses;
use disk_space_core::StorageTree;
use disk_space_core::format_bytes;
use nsh_base::button;
use nsh_base::input;
use nsh_core::prelude::ComponentBuilder;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::px;
use nsh_core::prelude::scroll;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::keybindings::Action;
use crate::palette;
use crate::state;

/// Renders the right-hand collapsible snag queue drawer when open.
#[expect(
    clippy::too_many_lines,
    reason = "snag drawer combines queue list, destruction mode selector, and safety warning"
)]
pub(crate) fn snag_drawer(tree: &StorageTree) -> View {
    let snag = state::snag_signal().get();
    if !snag.drawer_open {
        return view();
    }

    let items = snag.queue.items().to_vec();
    let total_bytes = snag.queue.total_bytes();
    let root_label = state::display_abs_path(tree.root().canonical_path());
    let is_cow_or_ssd = tree.volume().is_cow_or_ssd;

    let header = view()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            text(format!("destruction queue ({})", items.len()))
                .font_family(palette::mono_font())
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(12.0))
                .text_color(palette::TEXT_PRIMARY),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child((!items.is_empty()).then(|| {
                    button()
                        .accessibility_label("Clear queue")
                        .on_press(state::clear_snag_queue)
                        .cursor(Cursor::Pointer)
                        .child(
                            text("clear")
                                .font_family(palette::mono_font())
                                .text_size(px(11.0))
                                .text_color(palette::TEXT_MUTED),
                        )
                }))
                .child(
                    button()
                        .accessibility_label("Close queue")
                        .on_press(|| state::snag_signal().update(|s| s.drawer_open = false))
                        .cursor(Cursor::Pointer)
                        .child(
                            text("✕")
                                .font_family(palette::mono_font())
                                .text_size(px(12.0))
                                .text_color(palette::TEXT_MUTED),
                        ),
                ),
        );

    let jail_badge = view()
        .px_2()
        .py(px(4.0))
        .rounded_sm()
        .bg(palette::BG_INSET)
        .border_1()
        .border_color(palette::BORDER_SUBTLE)
        .child(
            text(format!("jailed to {root_label}"))
                .font_family(palette::mono_font())
                .text_size(px(10.0))
                .text_color(palette::SAFE_GREEN)
                .truncate(),
        );

    let mode_selector = view()
        .gap_2()
        .child(
            text("erasure mode")
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(palette::TEXT_FAINT),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap_1()
                .child(mode_pill(
                    "Trash",
                    matches!(snag.mode, DestructionMode::MoveToTrash),
                    DestructionMode::MoveToTrash,
                ))
                .child(mode_pill(
                    "Permanent",
                    matches!(snag.mode, DestructionMode::PermanentDelete),
                    DestructionMode::PermanentDelete,
                ))
                .child(mode_pill(
                    "Secure Erase",
                    matches!(snag.mode, DestructionMode::SecureErase { .. }),
                    DestructionMode::SecureErase {
                        passes: ErasePasses::ThreePassDod,
                    },
                )),
        )
        .child(match snag.mode {
            DestructionMode::SecureErase { passes } => Some(
                view()
                    .gap_2()
                    .child(
                        view()
                            .flex_row()
                            .gap_1()
                            .child(pass_pill(ErasePasses::SingleZero, passes))
                            .child(pass_pill(ErasePasses::ThreePassDod, passes))
                            .child(pass_pill(ErasePasses::SevenPass, passes)),
                    )
                    .child(if is_cow_or_ssd {
                        Some(
                            view()
                                .p_2()
                                .rounded_sm()
                                .bg(palette::WARN_SURFACE)
                                .child(
                                    text(
                                        "APFS/SSD note: CoW snapshots & wear-leveling may keep prior physical blocks until TRIM/snapshot purge.",
                                    )
                                    .font_family(palette::mono_font())
                                    .text_size(px(10.0))
                                    .text_color(palette::WARN_AMBER),
                                ),
                        )
                    } else {
                        None
                    }),
            ),
            DestructionMode::MoveToTrash | DestructionMode::PermanentDelete | _ => None,
        });

    let item_rows = view().gap_1().children(items.iter().map(|item| {
        let rel = item.jailed_path.relative_path().to_path_buf();
        let rel_str = rel.display().to_string();
        let accent = palette::category_color(item.category);
        view()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py(px(5.0))
            .rounded_sm()
            .bg(palette::BG_INSET)
            .gap_2()
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .gap_2()
                    .child(view().w(px(6.0)).h(px(6.0)).rounded_full().bg(accent))
                    .child(
                        text(rel_str)
                            .font_family(palette::mono_font())
                            .text_size(px(11.0))
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
                        text(format_bytes(item.bytes))
                            .font_family(palette::mono_font())
                            .tabular_nums()
                            .text_size(px(11.0))
                            .text_color(palette::TEXT_MUTED),
                    )
                    .child(
                        button()
                            .accessibility_label("Unstage item")
                            .on_press(move || state::unstage_item(&rel))
                            .cursor(Cursor::Pointer)
                            .child(
                                text("×")
                                    .font_family(palette::mono_font())
                                    .text_size(px(12.0))
                                    .text_color(palette::TEXT_FAINT),
                            ),
                    ),
            )
    }));

    let action_btn = button()
        .accessibility_label("Review and destroy snagged items")
        .disabled(items.is_empty() || snag.running)
        .on_press(state::open_destruction_review)
        .py_2()
        .px_3()
        .rounded_md()
        .items_center()
        .justify_center()
        .cursor(Cursor::Pointer)
        .bg(palette::DANGER_SURFACE)
        .border_1()
        .border_color(palette::DANGER_RED)
        .disabled_style(|s| s.opacity(0.4))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    text(format!("Review & Execute ({})", format_bytes(total_bytes)))
                        .font_family(palette::mono_font())
                        .font_weight(FontWeight::SEMIBOLD)
                        .tabular_nums()
                        .text_size(px(12.0))
                        .text_color(palette::DANGER_RED),
                )
                .child(
                    text(move || {
                        state::keybindings_signal()
                            .with(|kb| kb.primary_label(Action::ReviewAndExecute))
                            .unwrap_or_default()
                    })
                    .font_family(palette::mono_font())
                    .text_size(px(10.0))
                    .text_color(palette::TEXT_MUTED),
                ),
        );

    view()
        .w(px(320.0))
        .flex_shrink_0()
        .pl_4()
        .ml_2()
        .border_l_1()
        .border_color(palette::BORDER_SUBTLE)
        .gap_3()
        .child(header)
        .child(jail_badge)
        .child(mode_selector)
        .child(scroll().flex_1().child(if items.is_empty() {
            view().py_6().items_center().child(
                text("Press `s` or click `snag` to stage items")
                    .font_family(palette::mono_font())
                    .text_size(px(11.0))
                    .text_color(palette::TEXT_FAINT),
            )
        } else {
            item_rows
        }))
        .child(action_btn)
}

fn mode_pill(label: &'static str, active: bool, mode: DestructionMode) -> nsh_base::ButtonElement {
    button()
        .accessibility_label(format!("Mode {label}"))
        .on_press(move || state::snag_signal().update(|s| s.mode = mode))
        .px_2()
        .py(px(4.0))
        .rounded_sm()
        .cursor(Cursor::Pointer)
        .bg(if active {
            palette::DANGER_SURFACE
        } else {
            palette::BG_INSET
        })
        .border_1()
        .border_color(if active {
            palette::DANGER_RED
        } else {
            palette::BORDER_SUBTLE
        })
        .child(
            text(label)
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(if active {
                    palette::DANGER_RED
                } else {
                    palette::TEXT_MUTED
                }),
        )
}

fn pass_pill(passes: ErasePasses, active: ErasePasses) -> nsh_base::ButtonElement {
    let is_active = passes == active;
    button()
        .accessibility_label(passes.label())
        .on_press(move || {
            state::snag_signal().update(|s| {
                s.mode = DestructionMode::SecureErase { passes };
            });
        })
        .px_2()
        .py(px(3.0))
        .rounded_sm()
        .cursor(Cursor::Pointer)
        .bg(if is_active {
            palette::BG_SELECTED
        } else {
            palette::BG_INSET
        })
        .border_1()
        .border_color(if is_active {
            palette::DANGER_RED
        } else {
            palette::BORDER_SUBTLE
        })
        .child(
            text(passes.label())
                .font_family(palette::mono_font())
                .text_size(px(10.0))
                .text_color(if is_active {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }),
        )
}

/// Modal confirmation dialog protecting destructive operations and multi-pass secure erasure.
#[expect(
    clippy::too_many_lines,
    reason = "confirmation overlay presents target audit list, ERASE phrase check, and progress"
)]
pub(crate) fn confirm_modal() -> Option<View> {
    let snag = state::snag_signal().get();
    if !snag.confirm_open {
        return None;
    }

    let items = snag.queue.items().to_vec();
    let total_bytes = snag.queue.total_bytes();
    let requires_phrase = matches!(snag.mode, DestructionMode::SecureErase { .. });
    let phrase_ok = !requires_phrase || snag.erase_input.trim() == "ERASE";

    let mode_summary = match snag.mode {
        DestructionMode::MoveToTrash => "Move to Trash (recoverable)".to_owned(),
        DestructionMode::PermanentDelete => {
            "Permanent Delete (immediate unlink, bypasses Trash)".to_owned()
        }
        DestructionMode::SecureErase { passes } => format!(
            "Secure Erase ({} overwrite pass(es) + rename + unlink)",
            passes.count()
        ),
        _ => "Destruction".to_owned(),
    };

    let progress_banner = snag.progress.as_ref().map(|prog| {
        view().p_2().rounded_sm().bg(palette::BG_INSET).child(
            text(format!(
                "Destroying {}/{} ({}) · {}",
                prog.completed_items,
                prog.total_items,
                format_bytes(prog.reclaimed_bytes),
                prog.current_path
            ))
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(11.0))
            .text_color(palette::WARN_AMBER)
            .truncate(),
        )
    });

    let phrase_input = if requires_phrase {
        Some(
            view()
                .gap_1()
                .child(
                    text("Type `ERASE` to confirm multi-pass file destruction:")
                        .font_family(palette::mono_font())
                        .text_size(px(11.0))
                        .text_color(palette::WARN_AMBER),
                )
                .child(
                    input()
                        .value(move || state::snag_signal().with(|s| s.erase_input.clone()))
                        .on_change(move |val: &str| {
                            state::snag_signal().update(|s| val.clone_into(&mut s.erase_input));
                        })
                        .on_submit(|_: &str| state::confirm_execute_if_ready())
                        .placeholder("ERASE")
                        .px_3()
                        .py_2()
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .bg(palette::BG_INSET)
                        .border_1()
                        .border_color(palette::DANGER_RED)
                        .rounded_md()
                        .text_color(palette::TEXT_PRIMARY)
                        .placeholder_color(palette::TEXT_FAINT),
                ),
        )
    } else {
        None
    };

    Some(
        view()
            .absolute()
            .inset_0()
            .z_index(100)
            .bg(nsh_core::prelude::rgba(0x00_00_00_cc))
            .items_center()
            .justify_center()
            .p_6()
            .child(
                view()
                    .w(px(520.0))
                    .max_h(px(560.0))
                    .p_5()
                    .rounded_lg()
                    .bg(palette::BG_SURFACE)
                    .border_1()
                    .border_color(palette::BORDER_ACTIVE)
                    .gap_4()
                    .child(
                        view()
                            .gap_1()
                            .child(
                                text("Confirm Storage Destruction")
                                    .font_family(palette::mono_font())
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(px(15.0))
                                    .text_color(palette::TEXT_PRIMARY),
                            )
                            .child(
                                text(format!(
                                    "{mode_summary} · {} item(s) · {}",
                                    items.len(),
                                    format_bytes(total_bytes)
                                ))
                                .font_family(palette::mono_font())
                                .tabular_nums()
                                .text_size(px(12.0))
                                .text_color(palette::DANGER_RED),
                            ),
                    )
                    .child(
                        scroll()
                            .max_h(px(220.0))
                            .p_2()
                            .rounded_md()
                            .bg(palette::BG_INSET)
                            .border_1()
                            .border_color(palette::BORDER_SUBTLE)
                            .child(view().gap_1().children(items.iter().map(|item| {
                                view()
                                    .flex_row()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        text(item.jailed_path.as_path().display().to_string())
                                            .font_family(palette::mono_font())
                                            .text_size(px(11.0))
                                            .text_color(palette::TEXT_MUTED)
                                            .truncate(),
                                    )
                                    .child(
                                        text(format_bytes(item.bytes))
                                            .font_family(palette::mono_font())
                                            .tabular_nums()
                                            .text_size(px(11.0))
                                            .text_color(palette::TEXT_PRIMARY),
                                    )
                            }))),
                    )
                    .child(phrase_input)
                    .child(progress_banner)
                    .child(
                        view()
                            .flex_row()
                            .justify_end()
                            .gap_3()
                            .child(
                                button()
                                    .accessibility_label("Cancel destruction")
                                    .disabled(snag.running)
                                    .on_press(|| {
                                        state::snag_signal().update(|s| {
                                            s.confirm_open = false;
                                            s.erase_input.clear();
                                        });
                                    })
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(palette::BG_INSET)
                                    .border_1()
                                    .border_color(palette::BORDER_SUBTLE)
                                    .cursor(Cursor::Pointer)
                                    .child(
                                        text("Cancel")
                                            .font_family(palette::mono_font())
                                            .text_size(px(12.0))
                                            .text_color(palette::TEXT_MUTED),
                                    ),
                            )
                            .child(
                                button()
                                    .accessibility_label("Confirm destruction")
                                    .disabled(!phrase_ok || snag.running || items.is_empty())
                                    .on_press(state::run_staged_destruction)
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .bg(palette::DANGER_SURFACE)
                                    .border_1()
                                    .border_color(palette::DANGER_RED)
                                    .disabled_style(|s| s.opacity(0.4))
                                    .cursor(Cursor::Pointer)
                                    .child(
                                        text(if snag.running {
                                            "Executing..."
                                        } else {
                                            "Confirm Execute ↩"
                                        })
                                        .font_family(palette::mono_font())
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_size(px(12.0))
                                        .text_color(palette::DANGER_RED),
                                    ),
                            ),
                    ),
            ),
    )
}
