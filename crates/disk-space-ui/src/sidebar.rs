//! Left sidebar displaying the 9 storage categories and the active root safety jail status.

use disk_space_core::NodeRef;
use disk_space_core::StorageTree;
use disk_space_core::format_bytes;
use disk_space_platform::StorageCategory;
use nsh_base::button;
use nsh_core::prelude::ComponentBuilder;
use nsh_core::prelude::Cursor;
use nsh_core::prelude::FontWeight;
use nsh_core::prelude::ParentElement;
use nsh_core::prelude::Styled;
use nsh_core::prelude::View;
use nsh_core::prelude::px;
use nsh_core::prelude::text;
use nsh_core::prelude::view;

use crate::palette;
use crate::state;

/// Renders the left category breakdown sidebar and root safety-jail summary.
#[expect(
    clippy::too_many_lines,
    reason = "renders the 9-row category meter and safety-jail card in one cohesive panel"
)]
pub(crate) fn categories_sidebar(
    tree: &StorageTree,
    selected_category: Option<StorageCategory>,
) -> View {
    let root_node = tree.node(tree.root_id());
    let total_used = root_node.map_or(0, NodeRef::total_bytes).max(1);
    let max_category_bytes = StorageCategory::ALL
        .iter()
        .map(|&cat| root_node.map_or(0, |node| node.category_bytes(cat)))
        .max()
        .unwrap_or(1)
        .max(1);

    let all_selected = selected_category.is_none();

    let all_row = button()
        .accessibility_label("All categories")
        .on_press(|| state::select_category(None))
        .flex_row()
        .items_center()
        .justify_between()
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor(Cursor::Pointer)
        .bg(if all_selected {
            palette::BG_SELECTED
        } else {
            palette::BG_CANVAS
        })
        .hover(|s| s.bg(palette::BG_HOVER))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    text(if all_selected { "·" } else { " " })
                        .font_family(palette::mono_font())
                        .text_size(px(12.0))
                        .text_color(palette::TEXT_PRIMARY),
                )
                .child(
                    text("All items")
                        .font_family(palette::mono_font())
                        .font_weight(if all_selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_size(px(12.0))
                        .text_color(if all_selected {
                            palette::TEXT_PRIMARY
                        } else {
                            palette::TEXT_MUTED
                        }),
                ),
        )
        .child(
            text(format_bytes(root_node.map_or(0, NodeRef::total_bytes)))
                .font_family(palette::mono_font())
                .tabular_nums()
                .text_size(px(12.0))
                .text_color(if all_selected {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }),
        );

    let category_rows = StorageCategory::ALL.into_iter().map(move |category| {
        let bytes = root_node.map_or(0, |node| node.category_bytes(category));
        let is_selected = selected_category == Some(category);
        let accent = palette::category_color(category);
        #[expect(
            clippy::cast_precision_loss,
            reason = "visual dot bar fraction tolerates f32 precision"
        )]
        let share = (bytes as f32 / max_category_bytes as f32).clamp(0.0, 1.0);

        button()
            .accessibility_label(category.label())
            .on_press(move || {
                let current = state::browse_signal().with_untracked(|b| b.selected_category);
                if current == Some(category) {
                    state::select_category(None);
                } else {
                    state::select_category(Some(category));
                }
            })
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py(px(5.0))
            .rounded_sm()
            .cursor(Cursor::Pointer)
            .bg(if is_selected {
                palette::BG_SELECTED
            } else {
                palette::BG_CANVAS
            })
            .hover(|s| s.bg(palette::BG_HOVER))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        text(if is_selected { "·" } else { " " })
                            .font_family(palette::mono_font())
                            .text_size(px(12.0))
                            .text_color(accent),
                    )
                    .child(view().w(px(7.0)).h(px(7.0)).rounded(px(1.5)).bg(accent))
                    .child(
                        text(category.label())
                            .font_family(palette::mono_font())
                            .font_weight(if is_selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_size(px(12.0))
                            .text_color(if is_selected {
                                palette::TEXT_PRIMARY
                            } else {
                                palette::TEXT_MUTED
                            }),
                    ),
            )
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        text(format_bytes(bytes))
                            .font_family(palette::mono_font())
                            .tabular_nums()
                            .text_size(px(12.0))
                            .text_color(if is_selected {
                                palette::TEXT_PRIMARY
                            } else if bytes == 0 {
                                palette::TEXT_FAINT
                            } else {
                                palette::TEXT_MUTED
                            }),
                    )
                    .child(palette::mini_dot_bar(share, accent, 8)),
            )
    });

    let root_display = state::display_abs_path(tree.root().canonical_path());
    let file_count = root_node.map_or(0, NodeRef::file_count);
    let dir_count = root_node.map_or(0, NodeRef::dir_count);

    let jail_card = view()
        .mt_4()
        .p_3()
        .rounded_md()
        .bg(palette::BG_SURFACE)
        .border_1()
        .border_color(palette::BORDER_SUBTLE)
        .gap_1()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text("safety jail")
                        .font_family(palette::mono_font())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(11.0))
                        .text_color(palette::SAFE_GREEN),
                )
                .child(
                    text("active")
                        .font_family(palette::mono_font())
                        .text_size(px(10.0))
                        .text_color(palette::TEXT_FAINT),
                ),
        )
        .child(
            text(format!("root: {root_display}"))
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(palette::TEXT_MUTED)
                .truncate(),
        )
        .child(
            text("same-device lock · symlinks never followed")
                .font_family(palette::mono_font())
                .text_size(px(10.0))
                .text_color(palette::TEXT_FAINT),
        )
        .child(
            text(format!(
                "{file_count} files · {dir_count} dirs · {} symlinks skipped",
                tree.skipped_symlinks()
            ))
            .font_family(palette::mono_font())
            .tabular_nums()
            .text_size(px(10.0))
            .text_color(palette::TEXT_FAINT),
        );

    let _ = total_used;

    view()
        .w(px(276.0))
        .flex_shrink_0()
        .gap_1()
        .pr_4()
        .child(
            text("categories")
                .font_family(palette::mono_font())
                .text_size(px(11.0))
                .text_color(palette::TEXT_FAINT)
                .px_2()
                .pb_1(),
        )
        .child(all_row)
        .children(category_rows)
        .child(jail_card)
}
