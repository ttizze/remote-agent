use super::*;
pub(super) fn section_heading(title: &'static str, description: &'static str) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_lg().font_semibold().child(title))
        .child(
            div()
                .text_sm()
                .text_color(color("textMuted"))
                .child(description),
        )
}
impl Desktop {
    pub(super) fn view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let conversation = conversation(&self.snapshot);
        h_flex()
            .id("t3-conversation")
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if !view.settings
                    && view.composer.read(cx).focus_handle(cx).is_focused(window)
                    && event.keystroke.key == "enter"
                    && !event.keystroke.modifiers.shift
                {
                    let composer = agent_core::presentation::conversation(&view.snapshot).composer;
                    let behavior = if event.keystroke.modifiers.alt && composer.can_steer {
                        SendBehavior::Steer
                    } else {
                        SendBehavior::Default
                    };
                    if composer.enabled {
                        view.perform(Intent::Send { behavior }, None);
                    }
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .bg(color("canvas"))
            .text_color(color("text"))
            .text_size(px(14.))
            .child(self.sidebar(cx))
            .child(if self.settings {
                self.settings_view(window, cx)
            } else {
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.header(&conversation, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(self.timeline(&conversation, cx))
                                    .child(self.composer_view(&conversation, cx)),
                            )
                            .when(self.panel.is_some(), |row| row.child(self.panel_view(cx))),
                    )
                    .when(!self.error.is_empty(), |view| {
                        view.child(
                            h_flex()
                                .px_4()
                                .py_2()
                                .gap_2()
                                .bg(color("errorSurface"))
                                .text_color(color("errorForeground"))
                                .child(div().flex_1().child(
                                    agent_core::presentation::error::error_message(&self.error),
                                ))
                                .child(
                                    Button::new("dismiss-error")
                                        .label("Dismiss")
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.error.clear();
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .into_any_element()
            })
            .into_any_element()
    }
    fn sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let selected = self.snapshot.selected_project.clone();
        let entity = cx.entity().downgrade();
        let projects = self.snapshot.projects.clone();
        let title = selected
            .as_ref()
            .and_then(|id| projects.iter().find(|p| &p.id == id))
            .map_or("All projects", |p| p.name.as_str())
            .to_owned();
        let mut list = v_flex()
            .id("thread-shelves")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .gap(px(1.));
        if self.show_archive {
            for row in archived_threads(&self.snapshot, &now()) {
                list = list.child(self.thread_row(row, cx));
            }
        } else {
            for shelf in shelves(&self.snapshot, &now(), self.settled_limit) {
                let kind = shelf.kind;
                let collapsed = self.collapsed_shelves.contains(&kind);
                list = list.child(
                    Button::new(SharedString::from(format!("shelf-{kind:?}")))
                        .label(format!(
                            "{} {} {}",
                            if collapsed { "›" } else { "⌄" },
                            shelf.title,
                            shelf.total
                        ))
                        .small()
                        .ghost()
                        .h_8()
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if !view.collapsed_shelves.remove(&kind) {
                                view.collapsed_shelves.insert(kind);
                            }
                            cx.notify();
                        })),
                );
                if collapsed {
                    continue;
                }
                for row in shelf.rows {
                    list = list.child(self.thread_row(row, cx));
                }
                if shelf.has_more {
                    list = list.child(
                        Button::new("more-settled")
                            .label("Show 25 more")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.settled_limit += 25;
                                cx.notify();
                            })),
                    );
                }
            }
        }
        v_flex()
            .w(px(palette().sidebar_width))
            .flex_shrink_0()
            .h_full()
            .bg(color("sidebar"))
            .border_r_1()
            .border_color(color("sidebarBorder"))
            .child(
                h_flex()
                    .h(px(palette().header_height))
                    .px_3()
                    .gap_2()
                    .child(div().flex_1().font_semibold().child("Bex"))
                    .child(
                        Button::new("settings")
                            .label("Settings")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.settings = !view.settings;
                                view.hosts.update(cx, |hosts, cx| {
                                    hosts.refresh();
                                    cx.notify();
                                });
                                view.perform(Intent::LoadAccounts, None);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .px_2()
                    .gap_2()
                    .pb_2()
                    .child(Input::new(&self.search).h_8())
                    .child(
                        Button::new("project-picker")
                            .label(title)
                            .small()
                            .ghost()
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                let owner = entity.clone();
                                menu = menu.item(
                                    PopupMenuItem::new("All projects")
                                        .checked(selected.is_none())
                                        .on_click(move |_, _, cx| {
                                            let _ = owner.update(cx, |view, _| {
                                                view.perform(
                                                    Intent::FilterProject { project_id: None },
                                                    None,
                                                )
                                            });
                                        }),
                                );
                                for project in &projects {
                                    let owner = entity.clone();
                                    let id = project.id.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(project.name.clone())
                                            .checked(selected.as_ref() == Some(&project.id))
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |view, _| {
                                                    view.perform(
                                                        Intent::FilterProject {
                                                            project_id: Some(id.clone()),
                                                        },
                                                        None,
                                                    )
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                self.action(
                                    "new-thread",
                                    "New thread",
                                    Intent::NewThread {
                                        project_id: self.snapshot.selected_project.clone(),
                                    },
                                    cx,
                                )
                                .h(px(28.))
                                .flex_1(),
                            )
                            .child(
                                Button::new("add-project")
                                    .label("+")
                                    .small()
                                    .ghost()
                                    .tooltip("Add project folder")
                                    .on_click(cx.listener(|view, _, _, _| view.pick_folder())),
                            ),
                    ),
            )
            .child(list)
            .child(
                h_flex()
                    .px_2()
                    .py_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(color("sidebarBorder"))
                    .child(
                        Button::new("archives")
                            .label(if self.show_archive {
                                "Threads"
                            } else {
                                "Archived"
                            })
                            .small()
                            .ghost()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_archive = !view.show_archive;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("reconnect")
                            .label(if self.connecting {
                                "Connecting…"
                            } else if self.snapshot.connected {
                                "Connected"
                            } else {
                                "Reconnect"
                            })
                            .small()
                            .ghost()
                            .disabled(self.connecting)
                            .on_click(
                                cx.listener(|view, _, _, _| view.connect(view.remote.clone())),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn thread_row(&self, row: ThreadRow, cx: &Context<Self>) -> AnyElement {
        let id = row.id.clone();
        let menu_id = id.clone();
        let drop_id = id.clone();
        let owner = cx.entity().downgrade();
        let pinned = row.pinned;
        let archived = row.archived;
        let drag = PinnedDrag {
            id: id.clone(),
            title: row.title.clone(),
        };
        let status_color = match row.tone {
            StatusTone::Muted => "textMuted",
            StatusTone::Info => "updateForeground",
            StatusTone::Warning => "warningForeground",
            StatusTone::Input => "inputForeground",
            StatusTone::Error => "errorForeground",
            StatusTone::Success => "successForeground",
        };
        let title = div()
            .flex_1()
            .min_w_0()
            .text_ellipsis()
            .font_weight(if row.unread {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .child(row.title);
        let mut body = v_flex().w_full().min_w_0().gap_1();
        if row.slim {
            body = body.child(h_flex().gap_2().child(title).when_some(
                row.wake_label,
                |v, label| {
                    v.child(
                        div()
                            .text_size(px(11.))
                            .text_color(color("textMuted"))
                            .child(label),
                    )
                },
            ));
        } else {
            let project = self
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == row.project_id)
                .map_or("Chats", |project| project.name.as_str())
                .to_owned();
            body = body
                .child(
                    h_flex()
                        .h_5()
                        .gap_1()
                        .text_size(px(11.))
                        .text_color(color("textMuted"))
                        .child(div().flex_1().text_ellipsis().child(project))
                        .when(row.pinned, |v| v.child("⌖"))
                        .child(div().text_color(color(status_color)).child(row.status))
                        .when_some(row.duration_ms, |v, ms| v.child(format!("{}s", ms / 1000))),
                )
                .child(title)
                .child(
                    h_flex()
                        .gap_1()
                        .text_size(px(11.))
                        .text_color(color("textMuted"))
                        .child(
                            div()
                                .flex_1()
                                .text_ellipsis()
                                .child(row.branch.or(row.worktree).unwrap_or_default()),
                        )
                        .child(
                            Icon::default()
                                .path(if row.provider == "claude" {
                                    "bex/claude.svg"
                                } else {
                                    "bex/openai.svg"
                                })
                                .size(px(14.)),
                        ),
                );
        }
        div()
            .id(SharedString::from(format!("thread-{id}")))
            .w_full()
            .h(px(if row.slim { 36. } else { 82. }))
            .px(px(10.))
            .py_2()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|v| v.bg(color("sidebarRowHover")))
            .when(row.selected, |v| v.bg(color("sidebarRowSelected")))
            .child(body)
            .when(pinned && !archived, |view| {
                view.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                    .on_drop(cx.listener(move |view, drag: &PinnedDrag, _, _| {
                        view.perform(
                            Intent::ReorderPinned {
                                thread_id: drag.id.clone(),
                                before_thread_id: Some(drop_id.clone()),
                            },
                            None,
                        )
                    }))
            })
            .on_click(cx.listener(move |view, _, window, cx| {
                view.perform(
                    Intent::OpenThread {
                        thread_id: id.clone(),
                    },
                    None,
                );
                view.composer.read(cx).focus_handle(cx).focus(window, cx);
            }))
            .context_menu(move |mut menu, _, _| {
                let actions = vec![
                    (
                        if pinned { "Unpin" } else { "Pin" },
                        if pinned {
                            ThreadAction::Unpin
                        } else {
                            ThreadAction::Pin
                        },
                    ),
                    ("Settle", ThreadAction::Settle),
                    ("Mark unread", ThreadAction::MarkUnread),
                    (
                        if archived { "Unarchive" } else { "Archive" },
                        if archived {
                            ThreadAction::Unarchive
                        } else {
                            ThreadAction::Archive
                        },
                    ),
                ];
                for (label, action) in actions {
                    let owner = owner.clone();
                    let id = menu_id.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |view, _| {
                            view.perform(
                                Intent::Thread {
                                    thread_id: id.clone(),
                                    action: action.clone(),
                                },
                                None,
                            )
                        });
                    }));
                }
                if pinned {
                    for (label, up) in [("Move up", true), ("Move down", false)] {
                        let owner = owner.clone();
                        let id = menu_id.clone();
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |view, _| {
                                view.perform(
                                    Intent::MovePinned {
                                        thread_id: id.clone(),
                                        up,
                                    },
                                    None,
                                )
                            });
                        }));
                    }
                }
                menu
            })
            .into_any_element()
    }
    fn header(&self, chat: &ConversationView, cx: &Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        let selected = chat.thread_id.clone();
        let pinned = chat.pinned;
        let settled = chat.settled;
        let snoozed = chat.snoozed;
        let archived = chat.archived;
        let auto_settle = chat.auto_settle;
        let mut header = h_flex()
            .h(px(palette().header_height))
            .flex_shrink_0()
            .px_4()
            .gap_2()
            .border_b_1()
            .border_color(color("toolbarBorder"));
        if chat.can_merge_back {
            header = header.child(
                Button::new("merge-back")
                    .label("Merge back to source")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|view, _, _, _| view.perform(Intent::MergeBack, None))),
            );
        }
        if self.renaming {
            header = header.child(Input::new(&self.rename).flex_1()).child(
                Button::new("save-title")
                    .label("Save")
                    .small()
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.thread_action(ThreadAction::Rename {
                            title: view.rename.read(cx).value().to_string(),
                        });
                        view.renaming = false;
                        cx.notify();
                    })),
            );
        } else {
            header = header.child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(color("textMuted"))
                            .child(chat.project.clone()),
                    )
                    .child(
                        div()
                            .font_semibold()
                            .text_ellipsis()
                            .child(chat.title.clone()),
                    ),
            );
        }
        header = header.child(
            Button::new("thread-actions")
                .label("•••")
                .small()
                .ghost()
                .disabled(selected.is_none())
                .dropdown_menu(move |mut menu, _, _| {
                    let mut actions = vec![
                        (
                            if pinned { "Unpin" } else { "Pin" },
                            if pinned {
                                ThreadAction::Unpin
                            } else {
                                ThreadAction::Pin
                            },
                        ),
                        (
                            if settled { "Mark active" } else { "Settle" },
                            if settled {
                                ThreadAction::Unsettle
                            } else {
                                ThreadAction::Settle
                            },
                        ),
                        ("Mark unread", ThreadAction::MarkUnread),
                        (
                            if auto_settle {
                                "Disable auto settle"
                            } else {
                                "Enable auto settle"
                            },
                            ThreadAction::AutoSettle {
                                enabled: !auto_settle,
                            },
                        ),
                        (
                            if snoozed { "Unsnooze" } else { "Snooze 1 hour" },
                            if snoozed {
                                ThreadAction::Unsnooze
                            } else {
                                ThreadAction::Snooze {
                                    until: (chrono::Utc::now() + chrono::Duration::hours(1))
                                        .to_rfc3339(),
                                }
                            },
                        ),
                        (
                            if archived { "Unarchive" } else { "Archive" },
                            if archived {
                                ThreadAction::Unarchive
                            } else {
                                ThreadAction::Archive
                            },
                        ),
                        ("Delete", ThreadAction::Delete),
                    ];
                    for (label, action) in actions.drain(..) {
                        let owner = owner.clone();
                        let id = selected.clone();
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |view, _| {
                                if let Some(id) = &id {
                                    view.perform(
                                        Intent::Thread {
                                            thread_id: id.clone(),
                                            action: action.clone(),
                                        },
                                        None,
                                    );
                                }
                            });
                        }));
                    }
                    let owner = owner.clone();
                    menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            let title = conversation(&view.snapshot).title;
                            view.rename
                                .update(cx, |input, cx| input.set_value(title, window, cx));
                            view.renaming = true;
                            cx.notify();
                        });
                    }))
                }),
        );
        for (panel, label) in [
            (Panel::Diff, "Diff"),
            (Panel::Terminal, "Terminal"),
            (Panel::Files, "Files"),
            (Panel::Browser, "Browser"),
        ] {
            header = header.child(
                Button::new(SharedString::from(format!("panel-{label}")))
                    .label(label)
                    .small()
                    .ghost()
                    .selected(self.panel == Some(panel))
                    .on_click(
                        cx.listener(move |view, _, window, cx| view.open_panel(panel, window, cx)),
                    ),
            );
        }
        header.into_any_element()
    }
    fn timeline(&self, conversation: &ConversationView, cx: &Context<Self>) -> AnyElement {
        let mut content = v_flex()
            .w_full()
            .max_w(px(palette().chat_max_width))
            .mx_auto()
            .px_5()
            .py_6()
            .gap_6();
        if conversation.has_more_history {
            content = content.child(self.action(
                "load-history",
                "Load earlier messages",
                Intent::LoadHistory,
                cx,
            ));
        }
        if conversation.loading {
            content = content.child(
                div()
                    .text_color(color("textMuted"))
                    .child("Loading conversation…"),
            );
        }
        if conversation.rows.is_empty() && !conversation.loading {
            content = content.child(
                v_flex()
                    .py_12()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(24.))
                            .child("What would you like to build?"),
                    )
                    .child(
                        div()
                            .text_color(color("textMuted"))
                            .child("Choose a project and send a message."),
                    ),
            );
        }
        for row in &conversation.rows {
            content = content.child(self.timeline_row(row, cx));
        }
        div()
            .id("conversation-timeline")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.timeline_scroll)
            .child(content)
            .into_any_element()
    }
    fn timeline_row(&self, row: &TimelineRow, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .id(SharedString::from(format!("timeline-{}", row.id)))
            .anchor_scroll(self.anchors.get(&row.id).cloned())
            .w_full()
            .gap_2();
        match row.kind {
            RowKind::User => {
                body = body.items_end().child(
                    div()
                        .max_w_full()
                        .rounded(px(14.))
                        .px_4()
                        .py_3()
                        .bg(color("messageSurface"))
                        .child(
                            TextView::markdown(
                                SharedString::from(row.id.clone()),
                                row.text.clone(),
                            )
                            .selectable(true),
                        ),
                );
                if let Some(checkpoint) = &row.rollback_checkpoint_id {
                    let checkpoint = checkpoint.clone();
                    body = body.child(
                        Button::new(SharedString::from(format!("rollback-{}", row.id)))
                            .label("Edit from here")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |_, _, window, cx| {
                                let answer = window.prompt(
                                    gpui::PromptLevel::Warning,
                                    "Revert this thread?",
                                    Some("The conversation after this message will be rewound."),
                                    &["Cancel", "Revert files too", "Revert and keep changes"],
                                    cx,
                                );
                                let checkpoint = checkpoint.clone();
                                cx.spawn(async move |view, cx| {
                                    if let Ok(choice @ (1 | 2)) = answer.await {
                                        let _ = view.update(cx, |view, _| {
                                            view.perform(
                                                Intent::Rollback {
                                                    checkpoint_id: checkpoint,
                                                    restore_files: choice == 1,
                                                },
                                                None,
                                            )
                                        });
                                    }
                                })
                                .detach();
                            })),
                    );
                }
            }
            RowKind::Assistant => {
                if let Some(source) = &row.fork_source_thread_id
                    && let Some(run) = &row.run_id
                    && !row.streaming
                {
                    let source = source.clone();
                    let run = run.clone();
                    body = body.child(
                        Button::new(SharedString::from(format!("fork-{}", row.id)))
                            .label("Fork")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.perform(
                                    Intent::Fork {
                                        source_thread_id: source.clone(),
                                        run_id: run.clone(),
                                    },
                                    None,
                                )
                            })),
                    );
                }
                body = body
                    .child(
                        TextView::markdown(SharedString::from(row.id.clone()), row.text.clone())
                            .selectable(true),
                    )
                    .when(row.streaming, |v| {
                        v.child(div().text_color(color("textMuted")).child("●"))
                    });
            }
            RowKind::Work => {
                let id = row.id.clone();
                let expanded = self.expanded.contains(&id);
                body = body.child(
                    Button::new(SharedString::from(format!("expand-{id}")))
                        .label(format!(
                            "{} {} · {}",
                            if expanded { "⌄" } else { "›" },
                            row.title,
                            row.status
                        ))
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if !view.expanded.remove(&id) {
                                view.expanded.insert(id.clone());
                            }
                            cx.notify();
                        })),
                );
                if expanded {
                    for item in &row.work {
                        body = body.child(
                            v_flex()
                                .pl_4()
                                .gap_1()
                                .border_l_1()
                                .border_color(color("border"))
                                .child(
                                    div()
                                        .text_color(color("textMuted"))
                                        .child(format!("{} · {}", item.title, item.status)),
                                )
                                .child(
                                    TextView::markdown(
                                        SharedString::from(format!("work-{}", item.id)),
                                        item.detail.clone(),
                                    )
                                    .selectable(true),
                                ),
                        );
                    }
                }
            }
            RowKind::Approval | RowKind::Question => {
                body = body
                    .p_4()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(color("warning"))
                    .bg(color("warningSurface"))
                    .child(div().font_semibold().child(row.title.clone()));
                if !row.text.is_empty() {
                    body = body.child(
                        TextView::markdown(SharedString::from(row.id.clone()), row.text.clone())
                            .selectable(true),
                    );
                }
                if let Some(request) = &row.request_id {
                    for choice in &row.choices {
                        body = body.child(
                            v_flex()
                                .gap_1()
                                .when_some(choice.warning.clone(), |v, warning| {
                                    v.child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(color("warningForeground"))
                                            .child(warning),
                                    )
                                })
                                .child(
                                    self.action(
                                        SharedString::from(format!(
                                            "{}-{}",
                                            request, choice.decision
                                        )),
                                        choice.label.clone(),
                                        Intent::RespondApproval {
                                            request_id: request.clone(),
                                            decision: choice.decision.clone(),
                                        },
                                        cx,
                                    )
                                    .disabled(!row.actionable),
                                ),
                        );
                    }
                    for question in &row.questions {
                        body = body
                            .child(div().font_semibold().child(question.header.clone()))
                            .child(div().child(question.question.clone()));
                        let key = (request.clone(), question.id.clone());
                        if let Some(input) = self.questions.get(&key) {
                            for option in &question.options {
                                let value = option.value.clone();
                                let key = key.clone();
                                let multi = question.multi_select;
                                let selected = input.selected.contains(&value);
                                body = body.child(
                                    Button::new(SharedString::from(format!(
                                        "{}-{}-{}",
                                        request, question.id, value
                                    )))
                                    .label(format!(
                                        "{} {}{}",
                                        if selected { "●" } else { "○" },
                                        option.label,
                                        if option.description.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" — {}", option.description)
                                        }
                                    ))
                                    .small()
                                    .ghost()
                                    .disabled(!row.actionable)
                                    .selected(selected)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        if let Some(input) = view.questions.get_mut(&key) {
                                            if multi {
                                                if !input.selected.remove(&value) {
                                                    input.selected.insert(value.clone());
                                                }
                                            } else {
                                                input.selected.clear();
                                                input.selected.insert(value.clone());
                                                input.custom.update(cx, |input, cx| {
                                                    input.set_value("", window, cx)
                                                });
                                            }
                                        }
                                        cx.notify();
                                    })),
                                );
                            }
                            if question.allow_custom_answer || question.options.is_empty() {
                                body =
                                    body.child(Input::new(&input.custom).disabled(!row.actionable));
                            }
                        }
                    }
                    if !row.questions.is_empty() && !row.response_mode_message {
                        let request = request.clone();
                        let questions = row.questions.clone();
                        let validation =
                            question_error(questions.clone(), self.answers(&request, cx));
                        body = body
                            .child(
                                Button::new(SharedString::from(format!("answer-{request}")))
                                    .label("Submit answers")
                                    .primary()
                                    .disabled(!row.actionable)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let answers = view.answers(&request, cx);
                                        if let Some(error) =
                                            question_error(questions.clone(), answers.clone())
                                        {
                                            view.error = error;
                                            cx.notify();
                                        } else {
                                            view.perform(
                                                Intent::RespondQuestions {
                                                    request_id: request.clone(),
                                                    answers,
                                                },
                                                None,
                                            );
                                        }
                                    })),
                            )
                            .when_some(validation, |v, error| {
                                v.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(color("textMuted"))
                                        .child(error),
                                )
                            });
                    }
                    if row.response_mode_message {
                        body = body.child(
                            div()
                                .text_color(color("textMuted"))
                                .child("Reply in the composer"),
                        );
                    }
                    if row.response_mode_message {
                        body = body.child(
                            self.action(
                                SharedString::from(format!("dismiss-{request}")),
                                "Dismiss without answering",
                                Intent::DismissInput {
                                    request_id: request.clone(),
                                },
                                cx,
                            )
                            .disabled(!row.actionable),
                        );
                    }
                }
            }
            RowKind::Plan => {
                body = body
                    .p_4()
                    .border_1()
                    .border_color(color("border"))
                    .rounded(px(10.))
                    .child(div().font_semibold().child(row.title.clone()))
                    .child(
                        TextView::markdown(SharedString::from(row.id.clone()), row.text.clone())
                            .selectable(true),
                    );
            }
            _ => {
                body = body
                    .text_color(color(if row.kind == RowKind::Error {
                        "errorForeground"
                    } else {
                        "textMuted"
                    }))
                    .child(div().font_semibold().child(row.title.clone()))
                    .child(
                        TextView::markdown(SharedString::from(row.id.clone()), row.text.clone())
                            .selectable(true),
                    );
            }
        }
        body.into_any_element()
    }
    fn composer_view(&self, conversation: &ConversationView, cx: &Context<Self>) -> AnyElement {
        let composer = &conversation.composer;
        let mut content = v_flex()
            .w_full()
            .max_w(px(palette().chat_max_width))
            .mx_auto()
            .px_5()
            .pb_4()
            .gap_2();
        if !conversation.queue.is_empty() {
            let mut queue = v_flex()
                .gap_2()
                .rounded(px(10.))
                .bg(color("surface"))
                .border_1()
                .border_color(color("border"))
                .p_3()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(format!(
                            "{} queued{}",
                            composer.queue_count,
                            if composer.queue_held { " · held" } else { "" }
                        )))
                        .when(composer.queue_held, |v| {
                            v.child(self.action(
                                "resume-queue",
                                "Resume",
                                Intent::Queue {
                                    action: QueueAction::Resume,
                                },
                                cx,
                            ))
                        }),
                );
            let order = conversation
                .queue
                .iter()
                .map(|q| q.run_id.clone())
                .collect::<Vec<_>>();
            for (index, row) in conversation.queue.iter().enumerate() {
                let mut actions = h_flex()
                    .gap_1()
                    .child(self.action(
                        SharedString::from(format!("edit-{}", row.run_id)),
                        "Edit",
                        Intent::Queue {
                            action: QueueAction::Edit {
                                run_id: row.run_id.clone(),
                            },
                        },
                        cx,
                    ))
                    .when(row.can_steer, |v| {
                        v.child(self.action(
                            SharedString::from(format!("steer-{}", row.run_id)),
                            "Steer now",
                            Intent::Queue {
                                action: QueueAction::Steer {
                                    run_id: row.run_id.clone(),
                                },
                            },
                            cx,
                        ))
                    })
                    .child(self.action(
                        SharedString::from(format!("cancel-{}", row.run_id)),
                        "Cancel",
                        Intent::Queue {
                            action: QueueAction::Cancel {
                                run_id: row.run_id.clone(),
                            },
                        },
                        cx,
                    ));
                if index > 0 {
                    let mut reordered = order.clone();
                    reordered.swap(index, index - 1);
                    actions = actions.child(self.action(
                        SharedString::from(format!("up-{}", row.run_id)),
                        "↑",
                        Intent::Queue {
                            action: QueueAction::Reorder { run_ids: reordered },
                        },
                        cx,
                    ));
                }
                queue = queue.child(
                    h_flex()
                        .gap_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_ellipsis().child(row.text.clone()))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(color("textMuted"))
                                        .child(row.model.clone()),
                                ),
                        )
                        .child(actions),
                );
            }
            content = content.child(queue);
        }
        if let Some(notice) = &composer.notice {
            content = content.child(
                div()
                    .text_size(px(12.))
                    .text_color(color("warningForeground"))
                    .child(notice.clone()),
            );
        }
        let mut prompt = v_flex()
            .border_1()
            .border_color(color("input"))
            .rounded(px(14.))
            .bg(color("surface"))
            .p_2()
            .gap_2()
            .child(
                Textarea::new(&self.composer)
                    .bordered(false)
                    .disabled(conversation.archived)
                    .text_size(px(14.)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(self.model_picker(cx))
                    .child(self.runtime_picker(cx))
                    .child(self.interaction_picker(cx))
                    .child(div().flex_1())
                    .when(composer.editing, |v| {
                        v.child(self.action(
                            "cancel-queue-edit",
                            "Cancel edit",
                            Intent::Queue {
                                action: QueueAction::CancelEdit,
                            },
                            cx,
                        ))
                    })
                    .when(composer.can_stop, |v| {
                        v.child(self.action("stop-run", "Stop", Intent::Stop, cx))
                    })
                    .child(
                        self.action(
                            "send-message",
                            composer.send_label.clone(),
                            Intent::Send {
                                behavior: SendBehavior::Default,
                            },
                            cx,
                        )
                        .primary()
                        .disabled(!composer.enabled),
                    )
                    .when(composer.can_steer, |v| {
                        v.child(
                            self.action(
                                "steer-message",
                                "Steer",
                                Intent::Send {
                                    behavior: SendBehavior::Steer,
                                },
                                cx,
                            )
                            .disabled(!composer.enabled),
                        )
                    })
                    .when(composer.can_restart, |v| {
                        v.child(
                            self.action(
                                "restart-message",
                                "Restart",
                                Intent::Send {
                                    behavior: SendBehavior::Restart,
                                },
                                cx,
                            )
                            .disabled(!composer.enabled),
                        )
                    }),
            );
        if let Some(recording) = &self.dictation {
            prompt = prompt.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(recording.label))
                    .child(
                        div()
                            .h(px(4.))
                            .w(px(40. * recording.level))
                            .bg(color("accent")),
                    )
                    .child(
                        Button::new("finish-recording")
                            .label("Transcribe")
                            .small()
                            .disabled(!recording.recording)
                            .on_click(cx.listener(|view, _, _, _| view.finish_dictation())),
                    )
                    .child(
                        Button::new("cancel-recording")
                            .label("Cancel")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.cancel_recording();
                                cx.notify();
                            })),
                    ),
            );
        } else {
            prompt = prompt.child(
                h_flex()
                    .gap_2()
                    .child(Hosts::menu(
                        &self.hosts,
                        "composer-host",
                        self.remote.as_ref().map(|r| r.id.as_str()),
                        self.snapshot.host_name.as_deref().unwrap_or("Local"),
                        self.connecting,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(color("textMuted"))
                            .child(conversation.cwd.clone()),
                    )
                    .child(
                        Button::new("dictation")
                            .label("Dictate")
                            .small()
                            .ghost()
                            .disabled(!self.snapshot.connected)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.start_dictation();
                                cx.notify();
                            })),
                    ),
            );
        }
        content.child(prompt).into_any_element()
    }
    fn model_picker(&self, cx: &Context<Self>) -> AnyElement {
        let draft = self.snapshot.current_draft();
        let models = self.snapshot.models.clone();
        let owner = cx.entity().downgrade();
        Button::new("model-picker")
            .label(if draft.model.is_empty() {
                "Select model".into()
            } else {
                draft.model.clone()
            })
            .small()
            .ghost()
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for model in &models {
                    let owner = owner.clone();
                    let model = model.clone();
                    let checked = draft.model == model.id
                        && draft.instance_id
                            == match model.model.provider {
                                ProviderKind::Codex => "codex",
                                ProviderKind::Claude => "claude",
                            };
                    menu = menu.item(
                        PopupMenuItem::new(model.display_name.clone())
                            .checked(checked)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, _| {
                                    view.perform(
                                        Intent::SetModel {
                                            instance_id: match model.model.provider {
                                                ProviderKind::Codex => "codex",
                                                ProviderKind::Claude => "claude",
                                            }
                                            .into(),
                                            model: model.id.clone(),
                                            effort: Some(model.default_reasoning_effort.clone()),
                                            service_tier: model.default_service_tier.clone(),
                                        },
                                        None,
                                    )
                                });
                            }),
                    );
                }
                if let Some(model) = models.iter().find(|m| m.id == draft.model) {
                    menu = menu.separator();
                    for effort in &model.supported_reasoning_efforts {
                        let owner = owner.clone();
                        let value = effort.reasoning_effort.clone();
                        let draft = draft.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!("Reasoning: {value}"))
                                .checked(draft.effort.as_ref() == Some(&value))
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |view, _| {
                                        view.perform(
                                            Intent::SetModel {
                                                instance_id: draft.instance_id.clone(),
                                                model: draft.model.clone(),
                                                effort: Some(value.clone()),
                                                service_tier: draft.service_tier.clone(),
                                            },
                                            None,
                                        )
                                    });
                                }),
                        );
                    }
                    for tier in model.service_tiers.as_deref().unwrap_or_default() {
                        let owner = owner.clone();
                        let value = tier.id.clone();
                        let draft = draft.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!(
                                "Service: {}",
                                tier.name.as_deref().unwrap_or(&value)
                            ))
                            .checked(draft.service_tier.as_ref() == Some(&value))
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, _| {
                                    view.perform(
                                        Intent::SetModel {
                                            instance_id: draft.instance_id.clone(),
                                            model: draft.model.clone(),
                                            effort: draft.effort.clone(),
                                            service_tier: Some(value.clone()),
                                        },
                                        None,
                                    )
                                });
                            }),
                        );
                    }
                }
                menu
            })
            .into_any_element()
    }
    fn runtime_picker(&self, cx: &Context<Self>) -> AnyElement {
        let current = self.snapshot.current_draft().runtime_mode;
        let owner = cx.entity().downgrade();
        Button::new("runtime-mode")
            .label(current.clone())
            .small()
            .ghost()
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for mode in [
                    "approval-required",
                    "auto-accept-edits",
                    "auto",
                    "full-access",
                ] {
                    let owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(mode).checked(current == mode).on_click(
                        move |_, _, cx| {
                            let _ = owner.update(cx, |view, _| {
                                view.perform(Intent::SetRuntimeMode { mode: mode.into() }, None)
                            });
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }
    fn interaction_picker(&self, cx: &Context<Self>) -> AnyElement {
        let current = self.snapshot.current_draft().interaction_mode;
        let owner = cx.entity().downgrade();
        Button::new("interaction-mode")
            .label(if current == "plan" { "Plan" } else { "Default" })
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, _| {
                for mode in ["default", "plan"] {
                    let owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(mode).checked(current == mode).on_click(
                        move |_, _, cx| {
                            let _ = owner.update(cx, |view, _| {
                                view.perform(Intent::SetInteractionMode { mode: mode.into() }, None)
                            });
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }
    fn panel_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .w(px(palette().panel_width))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(color("border"));
        match self.panel {
            Some(Panel::Terminal) => {
                if let Some(terminal) = &self.terminal {
                    body = body.child(terminal.clone());
                }
            }
            Some(Panel::Browser) => {
                if let Some(browser) = &self.browser {
                    body = body.child(browser.clone());
                }
            }
            Some(Panel::Diff) => {
                let owner = cx.entity().downgrade();
                let cwd = self.snapshot.cwd();
                let options = self.snapshot.turn_diff_options();
                let refresh = self.snapshot.workspace.diff_request.as_ref().map_or_else(
                    || Intent::ReviewWorkspace { cwd: cwd.clone() },
                    |request| Intent::ReadTurnDiff {
                        from_turn_count: request.from_turn_count,
                        to_turn_count: request.to_turn_count,
                        ignore_whitespace: request.ignore_whitespace,
                    },
                );
                let label = self
                    .snapshot
                    .workspace
                    .review
                    .as_ref()
                    .map_or("Changes".to_owned(), |review| review.branch.clone());
                body = body
                    .child(
                        h_flex()
                            .h_10()
                            .px_3()
                            .child(
                                Button::new("diff-range")
                                    .label(label)
                                    .small()
                                    .ghost()
                                    .dropdown_caret(true)
                                    .dropdown_menu(move |mut menu, _, _| {
                                        let workspace_owner = owner.clone();
                                        let cwd = cwd.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new("Workspace changes").on_click(
                                                move |_, _, cx| {
                                                    let _ =
                                                        workspace_owner.update(cx, |view, _| {
                                                            view.perform(
                                                                Intent::ReviewWorkspace {
                                                                    cwd: cwd.clone(),
                                                                },
                                                                None,
                                                            )
                                                        });
                                                },
                                            ),
                                        );
                                        for option in &options {
                                            let owner = owner.clone();
                                            let from = option.from_turn_count;
                                            let to = option.to_turn_count;
                                            menu = menu.item(
                                                PopupMenuItem::new(option.label.clone()).on_click(
                                                    move |_, _, cx| {
                                                        let _ = owner.update(cx, |view, _| {
                                                            view.perform(
                                                                Intent::ReadTurnDiff {
                                                                    from_turn_count: from,
                                                                    to_turn_count: to,
                                                                    ignore_whitespace: false,
                                                                },
                                                                None,
                                                            )
                                                        });
                                                    },
                                                ),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .child(div().flex_1())
                            .child(self.action("refresh-diff", "Refresh", refresh, cx)),
                    )
                    .child(self.diff.clone());
            }
            Some(Panel::Files) => {
                body = body.child(
                    h_flex()
                        .p_2()
                        .gap_1()
                        .child(Input::new(&self.file_path).flex_1())
                        .child(Button::new("open-path").label("Open").small().on_click(
                            cx.listener(|view, _, _, cx| {
                                view.perform(
                                    Intent::ListFiles {
                                        path: view.file_path.read(cx).value().to_string(),
                                    },
                                    None,
                                )
                            }),
                        )),
                );
                if let Some(directory) = &self.snapshot.workspace.directory {
                    let parent = std::path::Path::new(&directory.path)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned());
                    let mut entries = v_flex()
                        .id("files-list")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .px_2();
                    if let Some(path) = parent {
                        entries = entries.child(self.action(
                            "parent-folder",
                            "..",
                            Intent::ListFiles { path },
                            cx,
                        ));
                    }
                    for entry in &directory.entries {
                        entries = entries.child(self.action(
                            SharedString::from(format!("file-{}", entry.path)),
                            format!("{}{}", if entry.directory { "▸ " } else { "" }, entry.name),
                            if entry.directory {
                                Intent::ListFiles {
                                    path: entry.path.clone(),
                                }
                            } else {
                                Intent::ReadFile {
                                    path: entry.path.clone(),
                                    discard_draft: false,
                                }
                            },
                            cx,
                        ));
                    }
                    body = body.child(entries);
                }
                if let Some(path) = &self.editor_path {
                    body = body
                        .child(
                            h_flex()
                                .px_3()
                                .h_10()
                                .gap_2()
                                .child(div().flex_1().text_ellipsis().child(path.clone()))
                                .child(self.action(
                                    "save-file",
                                    "Save",
                                    Intent::SaveFile { path: path.clone() },
                                    cx,
                                )),
                        )
                        .child(Editor::new(&self.editor).flex_1());
                }
            }
            _ => {}
        }
        body.into_any_element()
    }
    fn settings_view(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut content = v_flex()
            .w_full()
            .max_w(px(900.))
            .mx_auto()
            .p_6()
            .gap_6()
            .child(section_heading(
                "Settings",
                "Accounts and trusted connections",
            ));
        let current = self.remote.as_ref().map(|r| r.id.clone());
        let connected = self.snapshot.connected;
        content = content.child(self.hosts.update(cx, |hosts, cx| {
            hosts.connection_choices(current.as_deref(), connected, div(), cx)
        }));
        content = content.child(self.hosts.clone()).child(section_heading(
            "Provider accounts",
            "Credentials stay on the Host",
        ));
        if let Some(accounts) = &self.snapshot.accounts {
            if let Some(error) = &accounts.error {
                content = content.child(
                    div()
                        .text_color(color("errorForeground"))
                        .child(error.clone()),
                );
            }
            for account in &accounts.accounts {
                content = content.child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(format!(
                            "{:?} · {}",
                            account.provider,
                            account.email.as_deref().unwrap_or(&account.id)
                        )))
                        .child(self.action(
                            SharedString::from(format!("select-account-{}", account.id)),
                            if accounts.is_selected(account) {
                                "Selected"
                            } else {
                                "Use"
                            },
                            Intent::SelectAccount {
                                provider: account.provider,
                                id: account.id.clone(),
                            },
                            cx,
                        ))
                        .child(self.action(
                            SharedString::from(format!("delete-account-{}", account.id)),
                            "Remove",
                            Intent::DeleteAccount {
                                provider: account.provider,
                                id: account.id.clone(),
                            },
                            cx,
                        )),
                );
            }
        }
        content = content.child(
            h_flex()
                .gap_2()
                .child(self.action(
                    "login-codex",
                    "Connect Codex",
                    Intent::StartLogin {
                        provider: ProviderKind::Codex,
                    },
                    cx,
                ))
                .child(self.action(
                    "login-claude",
                    "Connect Claude",
                    Intent::StartLogin {
                        provider: ProviderKind::Claude,
                    },
                    cx,
                ))
                .child(self.action("refresh-accounts", "Refresh", Intent::LoadAccounts, cx)),
        );
        if let Some(login) = &self.snapshot.account_login {
            let url = login.verification_url.clone();
            let provider = login.provider;
            let id = login.login_id.clone();
            content = content.child(div().child(login.user_code.clone())).child(
                Button::new("open-login")
                    .label("Open sign-in page")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
            if login.requires_code_submission {
                content = content.child(Input::new(&self.account_code)).child(
                    Button::new("submit-login")
                        .label("Complete sign-in")
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.perform(
                                Intent::CompleteLogin {
                                    provider,
                                    id: id.clone(),
                                    code: view.account_code.read(cx).value().to_string(),
                                },
                                None,
                            )
                        })),
                );
            }
            content = content.child(self.action(
                "cancel-login",
                "Cancel",
                Intent::CancelLogin {
                    provider: login.provider,
                    id: login.login_id.clone(),
                },
                cx,
            ));
        }
        v_flex()
            .id("settings-page")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(content)
            .into_any_element()
    }
}
