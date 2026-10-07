//! The import step: choosing the folders whose Claude Code and Codex
//! sessions become projects and threads.
use crate::app::{
    Desktop,
    ui::{color, driver_icon, icon, now_ms, tint},
};
use agent_core::{
    state::Intent,
    view::projects::import::{ImportCandidateRow, ImportCheck, ImportToastKind, SessionScanStatus},
};
use agent_domain::Driver;
use gpui_kit::{
    component::{
        Disableable, Sizable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        notification::Notification,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::BTreeSet;

/// Scans the Host for sessions and opens the step.
pub(super) fn open(view: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>) {
    view.perform(Intent::ScanSessions);
    let desktop = cx.entity().downgrade();
    let step = cx.new(|_| ImportStep {
        desktop,
        collapsed: BTreeSet::new(),
        folders_open: false,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(672.))
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .child(step.clone())
    });
}

impl Desktop {
    /// Ends the step: shows the import's result once and lands where it put
    /// the threads.
    fn end_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(toast) = self.snapshot.import_toast() {
            let notification = match toast.kind {
                ImportToastKind::Success => {
                    Notification::success(toast.description.unwrap_or_default())
                }
                ImportToastKind::Warning => {
                    Notification::warning(toast.description.unwrap_or_default())
                }
            };
            window.push_notification(notification.title(toast.title), cx);
        }
        self.perform(Intent::CloseImport);
        window.close_dialog(cx);
        self.finish_import(window, cx);
    }
}

pub(super) struct ImportStep {
    desktop: WeakEntity<Desktop>,
    /// Repository groups the user folded.
    collapsed: BTreeSet<String>,
    folders_open: bool,
}

impl ImportStep {
    fn select(&self, paths: Vec<String>, checked: bool, cx: &mut Context<Self>) {
        let _ = self.desktop.update(cx, |view, _| {
            view.perform(Intent::SelectImportSessions { paths, checked })
        });
    }
}

fn checkbox(check: ImportCheck) -> Div {
    let filled = check != ImportCheck::Unchecked;
    h_flex()
        .size_4()
        .flex_shrink_0()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(if filled {
            color("messageAction")
        } else {
            color("input")
        })
        .when(filled, |box_| box_.bg(color("messageAction")))
        .when(check != ImportCheck::Unchecked, |box_| {
            box_.child(
                icon(if check == ImportCheck::Mixed {
                    "minus"
                } else {
                    "check"
                })
                .size_3()
                .text_color(color("messageActionForeground")),
            )
        })
}

/// Source icons, thread count and age in fixed columns.
fn meta(sources: &[Driver], threads: u64, age: &str) -> Div {
    let slot = |driver: Driver| {
        h_flex()
            .size_4()
            .justify_center()
            .when(sources.contains(&driver), |slot| {
                slot.child(driver_icon(driver).size_3())
            })
    };
    h_flex()
        .ml_auto()
        .flex_shrink_0()
        .gap_1()
        .text_xs()
        .text_color(color("textMuted"))
        .child(slot(Driver::Claude))
        .child(slot(Driver::Codex))
        .child(div().w(px(40.)).text_right().child(threads.to_string()))
        .child(
            div()
                .w(px(36.))
                .text_right()
                .whitespace_nowrap()
                .child(age.to_owned()),
        )
}

impl Render for ImportStep {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(desktop) = self.desktop.upgrade() else {
            return div().into_any_element();
        };
        let snapshot = desktop.read(cx).snapshot.clone();
        let importing = snapshot.session_import.importing;
        let view = snapshot.session_import(now_ms());
        let skip = Button::new("import-skip")
            .ghost()
            .label(view.skip_label.clone())
            .text_color(color("textMuted"))
            .disabled(!view.can_skip)
            .on_click(cx.listener(|step, _, window, cx| {
                let _ = step
                    .desktop
                    .update(cx, |view, cx| view.end_import(window, cx));
            }));
        if let SessionScanStatus::Loading { message } = &view.status {
            return v_flex()
                .min_h(px(160.))
                .child(div().text_2xl().font_semibold().child(view.title.clone()))
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .py_6()
                        .child(
                            icon("loader-circle")
                                .size_6()
                                .text_color(color("textMuted")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(color("textMuted"))
                                .child(message.clone()),
                        ),
                )
                .child(h_flex().justify_end().child(skip))
                .into_any_element();
        }
        let mut list = v_flex().gap(px(2.));
        for group in &view.repositories {
            if group.single {
                if let Some(row) = group.rows.first() {
                    list = list.child(self.candidate(row, importing, cx));
                }
                continue;
            }
            let open = !self.collapsed.contains(&group.key);
            let key = group.key.clone();
            let paths: Vec<String> = group.rows.iter().map(|row| row.path.clone()).collect();
            let check = group.check;
            list = list.child(
                h_flex()
                    .gap(px(10.))
                    .px_2()
                    .py(px(6.))
                    .rounded(px(6.))
                    .hover(|row| row.bg(tint("muted", 0.4)))
                    .child(
                        div()
                            .id(SharedString::from(format!("import-group-check-{key}")))
                            .cursor_pointer()
                            .child(checkbox(check))
                            .on_click(cx.listener(move |step, _, _, cx| {
                                if !importing {
                                    step.select(paths.clone(), check != ImportCheck::Checked, cx);
                                }
                            })),
                    )
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("import-group-{key}")))
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .cursor_pointer()
                            .child(
                                icon(if open {
                                    "chevron-down"
                                } else {
                                    "chevron-right"
                                })
                                .size(px(14.))
                                .text_color(color("textMuted")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_medium()
                                    .child(group.label.clone()),
                            )
                            .child(meta(&group.sources, group.thread_count, &group.age_label))
                            .on_click(cx.listener(move |step, _, _, cx| {
                                if !step.collapsed.remove(&key) {
                                    step.collapsed.insert(key.clone());
                                }
                                cx.notify();
                            })),
                    ),
            );
            if open {
                for row in &group.rows {
                    list = list.child(self.candidate(row, importing, cx));
                }
            }
        }
        if let Some(folders) = &view.folders {
            let paths: Vec<String> = folders.rows.iter().map(|row| row.path.clone()).collect();
            let check = folders.check;
            let open = self.folders_open;
            list = list.child(
                h_flex()
                    .gap(px(10.))
                    .px_2()
                    .py(px(6.))
                    .rounded(px(6.))
                    .hover(|row| row.bg(tint("muted", 0.4)))
                    .child(
                        div()
                            .id("import-folders-check")
                            .cursor_pointer()
                            .child(checkbox(check))
                            .on_click(cx.listener(move |step, _, _, cx| {
                                if !importing {
                                    step.select(paths.clone(), check != ImportCheck::Checked, cx);
                                }
                            })),
                    )
                    .child(
                        h_flex()
                            .id("import-folders")
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .cursor_pointer()
                            .child(
                                icon(if open {
                                    "chevron-down"
                                } else {
                                    "chevron-right"
                                })
                                .size(px(14.))
                                .text_color(color("textMuted")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(color("textMuted"))
                                    .child("Other folders"),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(color("textMuted"))
                                    .child(folders.label.clone()),
                            )
                            .on_click(cx.listener(|step, _, _, cx| {
                                step.folders_open = !step.folders_open;
                                cx.notify();
                            })),
                    ),
            );
            if open {
                for row in &folders.rows {
                    list = list.child(self.candidate(row, importing, cx));
                }
            }
        }
        let status = match &view.status {
            SessionScanStatus::Failed { message } => Some(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child(message.clone())
                    .child(
                        Button::new("import-retry")
                            .ghost()
                            .small()
                            .label("Retry")
                            .on_click(cx.listener(|step, _, _, cx| {
                                let _ = step
                                    .desktop
                                    .update(cx, |view, _| view.perform(Intent::ScanSessions));
                            })),
                    )
                    .into_any_element(),
            ),
            SessionScanStatus::Empty { message } => Some(
                div()
                    .py_2()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child(message.clone())
                    .into_any_element(),
            ),
            _ => None,
        };
        let all_paths: Vec<String> = view
            .repositories
            .iter()
            .flat_map(|group| &group.rows)
            .chain(view.folders.iter().flat_map(|folders| &folders.rows))
            .map(|row| row.path.clone())
            .collect();
        let selected_paths = view.selected_paths.clone();
        v_flex()
            .child(div().text_2xl().font_semibold().child(view.title.clone()))
            .child(
                div()
                    .mt(px(10.))
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child(view.description.clone()),
            )
            .when_some(view.selection_label.clone(), |step, label| {
                step.child(
                    h_flex()
                        .mt_5()
                        .justify_between()
                        .gap_3()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(label)
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("import-all")
                                        .ghost()
                                        .xsmall()
                                        .label("Select all")
                                        .disabled(!view.can_select_all)
                                        .on_click(cx.listener(move |step, _, _, cx| {
                                            step.select(all_paths.clone(), true, cx)
                                        })),
                                )
                                .child(
                                    Button::new("import-none")
                                        .ghost()
                                        .xsmall()
                                        .label("Select none")
                                        .disabled(!view.can_select_none)
                                        .on_click(cx.listener(move |step, _, _, cx| {
                                            step.select(selected_paths.clone(), false, cx)
                                        })),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .id("import-list")
                    .mt_2()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .pr_3()
                    .children(status)
                    .when_some(view.truncated_notice.clone(), |list, notice| {
                        list.child(div().text_xs().text_color(color("textMuted")).child(notice))
                    })
                    .child(list),
            )
            .child(
                h_flex().mt_6().justify_end().gap_3().child(skip).child(
                    Button::new("import-run")
                        .primary()
                        .label(view.import_label.clone())
                        .disabled(!view.can_import)
                        .on_click(cx.listener(|step, _, _, cx| {
                            let _ = step.desktop.update(cx, |view, _| {
                                view.perform_then(
                                    Intent::ImportSessions,
                                    |view, result, window, cx| match result {
                                        Ok(_) => view.end_import(window, cx),
                                        Err(error) => {
                                            let error = error.clone();
                                            view.show_error(&error, window, cx)
                                        }
                                    },
                                )
                            });
                        })),
                ),
            )
            .into_any_element()
    }
}

impl ImportStep {
    fn candidate(
        &self,
        row: &ImportCandidateRow,
        importing: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let path = row.path.clone();
        let checked = row.checked;
        h_flex()
            .id(SharedString::from(format!("import-row-{}", row.path)))
            .gap(px(10.))
            .px_2()
            .py(px(6.))
            .when(row.nested, |line| line.pl_8())
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|line| line.bg(tint("muted", 0.4)))
            .child(checkbox(if checked {
                ImportCheck::Checked
            } else {
                ImportCheck::Unchecked
            }))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .truncate()
                            .when(row.nested, |label| label.font_family("monospace").text_xs())
                            .when(!row.nested, |label| label.text_sm().font_medium())
                            .child(row.label.clone()),
                    )
                    .when_some(row.secondary.clone(), |line, secondary| {
                        line.child(
                            div()
                                .truncate()
                                .font_family("monospace")
                                .text_size(px(11.))
                                .text_color(color("textMuted"))
                                .child(secondary),
                        )
                    }),
            )
            .child(meta(&row.sources, row.thread_count, &row.age_label))
            .on_click(cx.listener(move |step, _, _, cx| {
                if !importing {
                    step.select(vec![path.clone()], !checked, cx);
                }
            }))
    }
}
