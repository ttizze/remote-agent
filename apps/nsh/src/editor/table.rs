//! A delimited file as T3 Code's table (`LargeTextViewer` over
//! `VirtualTablePreview`): how many rows and columns it has, its first row
//! heading the columns, and a numbered, striped row for each of the others.
use crate::l10n::{tr, tr_format};
use crate::ui::colors;
use agent_core::presentation::files::{FileTable, TABLE_ROW_HEADER_WIDTH};
use nsh_core::{
    element::{Rgba, rgba},
    prelude::*,
};

/// T3 Code's row: `min-h-[29px]`.
const ROW_HEIGHT: f32 = 29.0;
/// `bg-muted/30` under every other row.
const STRIPE: Rgba = rgba(0xffffff05);

pub(super) fn screen(path: Stored<String>, source: impl Fn() -> String + 'static) -> impl Element {
    let table = computed(move || FileTable::read(&source(), &path.get()));
    let viewport = signal((0.0_f32, 0.0_f32));
    let widths = computed(move || table.with(|table| table.column_widths(viewport.get().0)));
    let columns = move || (0..widths.with(Vec::len)).collect::<Vec<_>>();
    let width = move |column: usize| {
        px(widths.with(|widths| widths.get(column).copied().unwrap_or_default()))
    };
    let content_width = move || {
        (TABLE_ROW_HEADER_WIDTH + widths.with(|widths| widths.iter().sum::<f32>()))
            .max(viewport.get().0)
    };
    let empty = move || table.with(|table| table.rows.is_empty());
    let cell = move |text: Text, column: usize| {
        view()
            .w(move || width(column))
            .flex_shrink_0()
            .px_3()
            .py(px(6.0))
            .border_r_1()
            .border_r_color(colors::border())
            .child(text.text_size(px(12.0)).text_color(colors::foreground()))
    };
    let number = |label: Text| {
        view()
            .w(px(TABLE_ROW_HEADER_WIDTH))
            .flex_shrink_0()
            .px_2()
            .py(px(6.0))
            .items_end()
            .border_r_1()
            .border_r_color(colors::border())
            .child(label.text_size(px(12.0)).text_color(colors::muted()))
    };
    let head = move || {
        view()
            .flex_row()
            .min_h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .bg(colors::RAISED)
            .border_b_1()
            .border_b_color(colors::border())
            .child(number(text("")))
            .child(each(
                columns,
                |column: &usize| *column,
                move |column, _| {
                    let column = column.get_untracked();
                    let title = move || table.with(|table| table.column_title(column));
                    cell(
                        text(title)
                            .number_of_lines(1)
                            .font_weight(FontWeight::MEDIUM),
                        column,
                    )
                },
            ))
    };
    let rows = move || {
        virtual_list(
            move || table.with(|table| table.rows.len().saturating_sub(1)),
            move |at| {
                // The first row heads the columns: the list's rows follow it.
                let row = computed(move || {
                    table.with(|table| table.rows.get(at.index() + 1).cloned().unwrap_or_default())
                });
                view()
                    .flex_row()
                    .min_h(px(ROW_HEIGHT))
                    .border_b_1()
                    .border_b_color(colors::border())
                    .style_when(move || at.index() % 2 == 1, |s| s.bg(STRIPE))
                    .child(number(text(move || (at.index() + 1).to_string())))
                    .child(each(
                        columns,
                        |column: &usize| *column,
                        move |column, _| {
                            let column = column.get_untracked();
                            cell(
                                text(move || {
                                    row.with(|row| row.get(column).cloned().unwrap_or_default())
                                })
                                .selectable(true),
                                column,
                            )
                        },
                    ))
            },
        )
        .estimated_item_size(px(ROW_HEIGHT))
        .flex_1()
        .min_h_0()
    };
    view()
        .flex_1()
        .min_h_0()
        .child(
            view()
                .h(px(40.0))
                .flex_shrink_0()
                .justify_center()
                .px_3()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    text(move || {
                        table.with(|table| {
                            tr_format(
                                "{0} 行 · {1} 列",
                                [&table.rows.len().saturating_sub(1), &table.columns],
                            )
                        })
                    })
                    .text_size(px(12.0))
                    .text_color(colors::muted()),
                ),
        )
        .child(when(empty, || {
            view()
                .flex_1()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .child(
                    text(tr("このファイルは空です。"))
                        .text_size(px(12.0))
                        .text_color(colors::muted()),
                )
        }))
        .child(when(
            move || !empty(),
            move || {
                // The rows scroll down inside the area that scrolls sideways,
                // so they take the panel's height and the table's full width.
                view()
                    .flex_1()
                    .min_h_0()
                    .on_layout(move |frame: &Bounds<Points>| {
                        viewport.set((frame.size.width.0, frame.size.height.0))
                    })
                    .child(
                        scroll().horizontal().flex_1().min_h_0().child(
                            view()
                                .w(move || px(content_width()))
                                .h(move || px(viewport.get().1))
                                .child(head())
                                .child(rows()),
                        ),
                    )
            },
        ))
}
