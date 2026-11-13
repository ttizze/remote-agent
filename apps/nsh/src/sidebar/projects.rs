use crate::application::ListScope;
use crate::l10n::{tr, tr_format};
use crate::ui::{self, Icon, colors};
use crate::{Connection, threads::List};
use agent_core::presentation::{list::ProjectSummary, sidebar::ProjectKey};
use agent_core::state::Intent;
use nsh_core::{
    element::input,
    overlay::{Align, Placement},
    prelude::*,
};

pub(super) fn filter(list: List) -> impl IntoElement {
    let open = signal(false);
    let selected = list.selected;
    let chosen = computed(move || {
        list.content.with(|list| {
            list.as_ref()
                .map(|list| {
                    list.projects
                        .iter()
                        .filter(|project| {
                            selected.with(|selected| selected.contains(&key(project)))
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    let label = move || {
        SharedString::from(if chosen.with(Vec::is_empty) {
            tr("プロジェクトで絞り込む").to_owned()
        } else {
            tr_format(
                "プロジェクトで絞り込む: {0}",
                [&chosen.with(|projects| {
                    projects
                        .iter()
                        .map(|project| project.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })],
            )
        })
    };
    nsh_base::popover()
        .open(move || Some(open.get()))
        .on_open_change(move |event: &nsh_base::PopoverEvent| {
            if let nsh_base::PopoverEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .placement(Placement::Bottom)
        .align(Align::Start)
        .offset(px(4.0))
        .margin(px(8.0))
        .popup_class("bg-[#0f0f0f] rounded-[10px] border border-[#303030]")
        .trigger(
            ui::bare_button(label, || {})
                .selected(move || !selected.with(Vec::is_empty))
                .selected_style(|s| s.bg(colors::ROW_HOVER))
                .size(px(28.0))
                .rounded(px(ui::RADIUS))
                .text_color(colors::muted())
                .hover(|s| s.bg(colors::ROW_HOVER))
                .child(view().tooltip(ui::tooltip(label)).child(dynamic(move || {
                    let projects = chosen.get();
                    if projects.is_empty() {
                        return ui::icon(Icon::FILTER, 16.0).into_element();
                    }
                    projects
                        .iter()
                        .take(2)
                        .enumerate()
                        .fold(view().flex_row().items_center(), |row, (index, project)| {
                            row.child(
                                view()
                                    .ml(px(if index == 0 { 0.0 } else { -4.0 }))
                                    .child(project_icon(project, 16.0)),
                            )
                        })
                        .into_element()
                }))),
        )
        .child(when(
            move || open.get(),
            move || panel(list, selected, open),
        ))
}

fn key(project: &ProjectSummary) -> ProjectKey {
    ProjectKey {
        host_id: project.host_id.clone(),
        project_id: project.id.clone(),
    }
}

fn project_icon(project: &ProjectSummary, size: f32) -> AnyElement {
    crate::threads::project_icon(
        project.icon.as_ref().map(|icon| icon.dark_png.as_slice()),
        Some((project.monogram.clone(), project.icon_color)),
        size,
    )
}

fn panel(list: List, selected: Memo<Vec<ProjectKey>>, open: Signal<bool>) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let query = signal(String::new());
    let focused = signal(false);
    let highlighted = signal(0_usize);
    let order = stored(Vec::<ProjectKey>::new());
    let choose = move |project: Option<ProjectKey>, additive: bool| {
        let additive = additive && project.is_some();
        let next = agent_core::presentation::sidebar::select_projects(
            &selected.get_untracked(),
            project,
            additive,
        );
        connection
            .application
            .with(|app| app.select_sidebar_projects(next));
        if !additive {
            open.set(false);
        }
    };
    let projects = computed(move || {
        list.content.with(|list| {
            list.as_ref()
                .map(|list| {
                    let ordered = order.with(|order| {
                        let (next, ordered) = agent_core::presentation::sidebar::order_projects(
                            &list.projects,
                            order,
                        );
                        *order = next;
                        ordered
                    });
                    agent_core::presentation::sidebar::matching_projects(ordered, &query.get())
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    let confirm = move |additive: bool| {
        let index = highlighted.get_untracked();
        if index == 0 {
            if !additive {
                choose(None, false);
            }
        } else if let Some(project) =
            projects.with_untracked(|projects| projects.get(index - 1).map(key))
        {
            choose(Some(project), additive);
        }
    };
    view()
        .w(px(288.0))
        .max_w(move || px((window().width.0 - 16.0).max(0.0)))
        .text_color(colors::foreground())
        .child(when(
            move || focused.get(),
            move || {
                for key in ["up", "down"] {
                    on_key_with(
                        key,
                        KeyOptions {
                            when: nsh_core::keymap::When::Always,
                            repeat: true,
                            ..Default::default()
                        },
                        move |_| {
                            if let Some(agent_core::composer::CompletionKey::Highlight(index)) =
                                agent_core::composer::completion_key(
                                    key,
                                    highlighted.get_untracked(),
                                    projects.with_untracked(Vec::len) + 1,
                                )
                            {
                                highlighted.set(index);
                            }
                        },
                    );
                }
                on_key_with(
                    "shift+enter",
                    KeyOptions {
                        when: nsh_core::keymap::When::Always,
                        ..Default::default()
                    },
                    move |_| confirm(true),
                );
                view()
            },
        ))
        .child(
            input()
                .auto_focus(true)
                .auto_correct(false)
                .auto_capitalize(AutoCapitalize::None)
                .label(tr("プロジェクトを検索"))
                .placeholder(tr("プロジェクトを検索…"))
                .value(move || query.get())
                .on_focus(move || focused.set(true))
                .on_blur(move || focused.set(false))
                .submit_behavior(SubmitBehavior::Submit)
                .on_submit(move |_: &str| confirm(false))
                .on_change(move |text: &str| {
                    query.set(text.to_owned());
                    highlighted.set(usize::from(!text.trim().is_empty()));
                })
                .text_size(px(14.0))
                .text_color(colors::foreground())
                .placeholder_color(colors::muted())
                .h(px(40.0))
                .px_3()
                .border_b_1()
                .border_b_color(colors::border()),
        )
        .child(
            scroll().max_h(px(320.0)).child(
                view()
                    .p_1()
                    .gap_1()
                    .child(
                        scope_row(
                            tr("すべてのプロジェクト").to_owned(),
                            move || selected.with(Vec::is_empty),
                            move |additive| choose(None, additive),
                        )
                        .style_when(
                            move || focused.get() && highlighted.get() == 0,
                            |s| s.bg(colors::ROW_HOVER),
                        )
                        .child(ui::icon(Icon::FOLDER, 16.0))
                        .child(text(tr("すべてのプロジェクト")).text_size(px(14.0))),
                    )
                    .child(when(
                        move || projects.with(Vec::is_empty),
                        move || {
                            text(tr("一致するプロジェクトはありません"))
                                .px_3()
                                .py_3()
                                .text_size(px(14.0))
                                .text_color(colors::muted())
                        },
                    ))
                    .child(each(
                        move || projects.get(),
                        key,
                        move |project, position| {
                            dynamic(move || {
                                let row = element_ref();
                                effect(move || {
                                    if focused.get() && highlighted.get() == position.get() + 1 {
                                        row.reveal(Default::default());
                                    }
                                });
                                let project = project.get();
                                let project_key = key(&project);
                                let checked = project_key.clone();
                                let scope = list.sources.with(|sources| {
                                    sources
                                        .iter()
                                        .find(|source| source.scope.host == project.host_id)
                                        .map(|source| source.scope.clone())
                                });
                                view()
                                    .flex_row()
                                    .items_center()
                                    .child(
                                        scope_row(
                                            project.name.clone(),
                                            move || {
                                                selected
                                                    .with(|selected| selected.contains(&checked))
                                            },
                                            move |additive| {
                                                choose(Some(project_key.clone()), additive)
                                            },
                                        )
                                        .element_ref(&row)
                                        .style_when(
                                            move || {
                                                focused.get()
                                                    && highlighted.get() == position.get() + 1
                                            },
                                            |s| s.bg(colors::ROW_HOVER),
                                        )
                                        .flex_1()
                                        .min_w_0()
                                        .child(project_icon(&project, 16.0))
                                        .child(
                                            text(project.name.clone())
                                                .text_size(px(14.0))
                                                .truncate()
                                                .flex_1()
                                                .min_w_0(),
                                        )
                                        .child(when(
                                            move || connection.state.with(|state| state.all_hosts),
                                            {
                                                let host = project.host_id.clone();
                                                move || {
                                                    text(connection.state.with(|state| {
                                                        state
                                                            .hosts
                                                            .iter()
                                                            .find(|candidate| candidate.id == host)
                                                            .map(|host| host.label())
                                                            .unwrap_or_default()
                                                    }))
                                                    .text_size(px(11.0))
                                                    .text_color(colors::muted())
                                                    .truncate()
                                                    .max_w(px(72.0))
                                                }
                                            },
                                        )),
                                    )
                                    .child(scope.map(|scope| {
                                        list.sections.with(|sections| {
                                            project_actions(
                                                &project,
                                                scope,
                                                sections,
                                                list.projects,
                                            )
                                        })
                                    }))
                            })
                        },
                    )),
            ),
        )
}

fn scope_row(
    label: String,
    selected: impl Fn() -> bool + 'static,
    choose: impl Fn(bool) + 'static,
) -> View {
    let selected = computed(selected);
    view()
        .role(AccessibilityRole::Button)
        .label(SharedString::from(label))
        .accessibility_state(move || nsh_core::AccessibilityState {
            selected: selected.get(),
            ..Default::default()
        })
        .on_press(move |event: &PressEvent| choose(event.modifiers.shift))
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(32.0))
        .px_2()
        .rounded(px(6.0))
        .hover(|s| s.bg(colors::ROW_HOVER))
        .focus_visible(|s| s.bg(colors::ROW_HOVER))
        .child(
            view()
                .w(px(12.0))
                .child(when(move || selected.get(), || ui::icon(Icon::CHECK, 12.0))),
        )
}

fn project_actions(
    project: &ProjectSummary,
    scope: ListScope,
    sections: &[String],
    projects: crate::projects::Projects,
) -> View {
    let connection = use_context::<Connection>().unwrap();
    let id = project.id.clone();
    let pinned = project.pinned;
    let title = tr_format("{0}のプロジェクト設定", [&project.name]);
    let menu = crate::projects::menu(project, sections, &scope, scope.host == "local");
    view()
        .flex_row()
        .items_center()
        .child(
            ui::small_icon_button(
                Icon::PIN,
                format!(
                    "{}: {}",
                    if pinned {
                        tr("ピン留めを解除")
                    } else {
                        tr("ピン留め")
                    },
                    project.name
                ),
                move || {
                    connection.application.with(|app| {
                        app.list_action(
                            &scope,
                            Intent::SetProjectPinned {
                                project_id: id.clone(),
                                pinned: !pinned,
                            },
                            false,
                        )
                    });
                },
            )
            .text_color(if pinned {
                colors::foreground()
            } else {
                colors::muted()
            }),
        )
        .child(
            ui::small_icon_button(Icon::SETTINGS, title, move || {
                ui::popup(&menu, move |command| projects.select(command));
            })
            .text_color(colors::muted()),
        )
}
