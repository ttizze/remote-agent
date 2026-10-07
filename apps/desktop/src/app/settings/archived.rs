//! The Archive page: archived threads by project, to unarchive or delete.
use super::{Row, notice, page_container, section};
use crate::app::{
    Desktop,
    ui::{color, icon, now_ms},
};
use agent_core::{
    state::Intent,
    view::archived::{ArchivedActionKind, ArchivedLayout, ArchivedOptions},
};
use gpui_kit::{
    component::{
        Sizable,
        button::Button,
        h_flex,
        menu::{ContextMenuExt, PopupMenuItem},
    },
    prelude::FluentBuilder,
    *,
};

impl Desktop {
    pub(super) fn render_archived(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self.snapshot.archived(
            now_ms(),
            ArchivedOptions {
                layout: ArchivedLayout::Settings,
                confirm_delete: true,
                ..ArchivedOptions::default()
            },
        );
        if let Some(empty) = view.empty {
            let mut row = Row::new(
                h_flex()
                    .gap_2()
                    .child(
                        icon(if empty.loading {
                            "loader-circle"
                        } else {
                            "archive"
                        })
                        .size(px(14.))
                        .text_color(color("textMuted")),
                    )
                    .child(empty.title),
            );
            if let Some(detail) = empty.detail {
                row = row.description(detail);
            }
            return page_container(
                896.,
                vec![
                    section(
                        Some("Archived threads".into()),
                        None,
                        None,
                        vec![row.render()],
                    )
                    .into_any_element(),
                ],
            );
        }
        let mut sections = Vec::new();
        if let Some(error) = &view.error {
            sections.push(notice(error.clone()));
        }
        for group in view.groups {
            let rows = group
                .rows
                .into_iter()
                .map(|row| {
                    let thread_id = row.thread_id.clone();
                    let unarchive = row
                        .actions
                        .iter()
                        .find(|action| action.kind == ArchivedActionKind::Unarchive)
                        .cloned();
                    let actions = row.actions.clone();
                    let owner = cx.entity().downgrade();
                    let menu_thread = thread_id.clone();
                    div()
                        .id(SharedString::from(format!("archived-{thread_id}")))
                        .child(
                            Row::new(row.title.clone())
                                .description(row.description.clone())
                                .when_some(unarchive, |setting, action| {
                                    let thread_id = thread_id.clone();
                                    setting.control(
                                        Button::new(SharedString::from(format!(
                                            "unarchive-{thread_id}"
                                        )))
                                        .outline()
                                        .xsmall()
                                        .icon(icon("archive-x"))
                                        .label(action.label.clone())
                                        .on_click(
                                            cx.listener(move |view, _, _, _| {
                                                view.perform(Intent::Thread {
                                                    thread_id: thread_id.clone(),
                                                    action: action.action.clone(),
                                                });
                                            }),
                                        ),
                                    )
                                })
                                .render(),
                        )
                        .context_menu(move |mut menu, _, _| {
                            for action in &actions {
                                let owner = owner.clone();
                                let thread_id = menu_thread.clone();
                                let action = action.clone();
                                menu =
                                    menu.item(PopupMenuItem::new(action.label.clone()).on_click(
                                        move |_, window, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.perform_confirmed(
                                                    Intent::Thread {
                                                        thread_id: thread_id.clone(),
                                                        action: action.action.clone(),
                                                    },
                                                    action.confirmation.as_ref(),
                                                    &action.label,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                    ));
                            }
                            menu
                        })
                        .into_any_element()
                })
                .collect();
            sections.push(
                section(
                    Some(group.title.into()),
                    Some(
                        icon("folder")
                            .size(px(14.))
                            .text_color(color("textMuted"))
                            .into_any_element(),
                    ),
                    None,
                    rows,
                )
                .into_any_element(),
            );
        }
        page_container(896., sections)
    }
}
