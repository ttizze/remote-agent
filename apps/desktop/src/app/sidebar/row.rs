//! One sidebar entry each: thread rows (card and slim), unsent drafts,
//! search results, shelf headers, drag boundaries and "Show more".
use super::{
    super::{
        Desktop,
        menus::MenuSurface,
        ui::{self, color, driver_icon, icon, tint},
    },
    drag::{DragPreview, ThreadDrag},
    project_mark,
};
use agent_core::{
    state::{Intent, ThreadAction},
    view::{
        models::ModelCatalog,
        search::highlight_parts,
        sidebar::{
            SidebarDraftRow, SidebarListMarker, SidebarMatchSource, SidebarRowAction,
            SidebarRowSurface, SidebarRowTrailing, SidebarRowVariant, SidebarSearchResult,
            SidebarSection, SidebarShelfHeader, SidebarThreadRow, SidebarTitleTone,
            SidebarTopStatus, sidebar_marker_id,
        },
    },
};
use gpui_kit::{
    component::{h_flex, tooltip::Tooltip, v_flex},
    prelude::FluentBuilder,
    *,
};
use std::{f32::consts::PI, sync::Arc, time::Duration};

/// The status icon and its color.
fn status_style(status: SidebarTopStatus) -> (Option<&'static str>, Hsla) {
    match status {
        SidebarTopStatus::Working => (Some("circle-dashed"), color("updateForeground")),
        SidebarTopStatus::Waiting => (None, color("textMuted")),
        SidebarTopStatus::Approval => (Some("shield-question"), color("warningForeground")),
        SidebarTopStatus::Input => (Some("message-circle-question"), color("inputForeground")),
        SidebarTopStatus::Limited => (Some("circle-alert"), color("warning")),
        SidebarTopStatus::Failed => (Some("circle-alert"), color("error")),
        SidebarTopStatus::Woke => (Some("alarm-clock"), color("warning")),
        SidebarTopStatus::Done => (Some("circle-check"), color("successForeground")),
    }
}

fn title_color(tone: SidebarTitleTone, hovered: bool) -> Hsla {
    match tone {
        SidebarTitleTone::Prominent => color("text"),
        _ if hovered => color("text"),
        SidebarTitleTone::Normal => tint("text", 0.9),
        SidebarTitleTone::Failed => tint("text", 0.95),
        SidebarTitleTone::Muted => color("textMuted"),
        SidebarTitleTone::Secondary => color("secondaryLabel"),
        SidebarTitleTone::SecondaryDim => tint("secondaryLabel", 0.7),
    }
}

/// A small text button inside a row; it never opens the row.
fn row_button(id: impl Into<ElementId>, tooltip: &'static str) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h_full()
        .gap_1()
        .px_1p5()
        .rounded(px(8.))
        .cursor_pointer()
        .text_xs()
        .text_color(color("textMuted"))
        .hover(|button| button.text_color(color("text")))
        .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
}

fn element_id(kind: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{kind}-{key}").into())
}

/// The row's hover card: the title, then its project, branch and provider,
/// and the providers it was handed off from.
fn hover_card(
    row: &SidebarThreadRow,
    catalog: &ModelCatalog,
    project_image: Option<Arc<Image>>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let title = row.title.clone();
    let terminals = row.terminal_processes.clone();
    let project = row.project_name.clone();
    let branch = row.branch.clone();
    let current_id = row.provider_stack.last().cloned();
    let current = current_id
        .as_deref()
        .and_then(|id| catalog.instance(id))
        .map(|instance| (instance.driver, instance.display_name.clone()));
    let earlier: Vec<String> = row
        .provider_stack
        .iter()
        .filter(|id| Some(*id) != current_id.as_ref())
        .map(|id| {
            catalog
                .instance(id)
                .map_or_else(|| id.clone(), |instance| instance.display_name.clone())
        })
        .collect();
    move |window, cx| {
        let (title, project, branch, current, earlier, terminals, project_image) = (
            title.clone(),
            project.clone(),
            branch.clone(),
            current.clone(),
            earlier.clone(),
            terminals.clone(),
            project_image.clone(),
        );
        Tooltip::element(move |_, _| {
            let line = || h_flex().min_w_0().gap_2();
            v_flex()
                .max_w(px(320.))
                .gap_2()
                .px_1()
                .py_2()
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color("text"))
                        .child(title.clone()),
                )
                .child(
                    v_flex()
                        .gap_1p5()
                        .pl_0p5()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .when_some(project.clone(), |card, name| {
                            card.child(
                                line()
                                    .child(project_mark(project_image.clone(), &name, 12.))
                                    .child(
                                        div().truncate().text_color(tint("text", 0.75)).child(name),
                                    ),
                            )
                        })
                        .when_some(branch.clone(), |card, branch| {
                            card.child(
                                line()
                                    .child(icon("git-branch").size_3())
                                    .child(div().truncate().child(branch)),
                            )
                        })
                        .when_some(current.clone(), |card, (driver, name)| {
                            card.child(
                                line()
                                    .child(driver_icon(driver).size_3().opacity(0.6))
                                    .child(
                                        div().truncate().text_color(tint("text", 0.75)).child(name),
                                    ),
                            )
                        })
                        .when_some(terminals.clone(), |card, terminals| {
                            card.child(
                                line()
                                    .child(
                                        icon("terminal").size_3().text_color(color("statusTeal")),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_color(tint("text", 0.75))
                                            .child(terminals),
                                    ),
                            )
                        })
                        .when(!earlier.is_empty(), |card| {
                            card.child(
                                line().child(icon("arrow-right-left").size_3()).child(
                                    div()
                                        .truncate()
                                        .text_color(tint("text", 0.75))
                                        .child(format!("Handed off from {}", earlier.join(", "))),
                                ),
                            )
                        }),
                )
        })
        .build(window, cx)
    }
}

/// The pulse of the running-terminal icon: full for 40% of its 2 s, half
/// for the next 40%, stepping between them in six steps.
fn terminal_pulse_opacity(progress: f32) -> f32 {
    let step = |from: f32, to: f32, start: f32| {
        let steps = (((progress - start) / 0.1).clamp(0., 1.) * 6.).floor() / 6.;
        from + (to - from) * steps
    };
    match progress {
        p if p < 0.4 => 1.,
        p if p < 0.5 => step(1., 0.5, 0.4),
        p if p < 0.9 => 0.5,
        _ => step(0.5, 1., 0.9),
    }
}

/// The teal terminal icon of a thread whose terminals run a process.
fn terminal_indicator(id: &str, label: Option<String>) -> Option<AnyElement> {
    let label: SharedString = label?.into();
    Some(
        div()
            .id(element_id("terminal-running", id))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .text_color(color("statusTeal"))
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .child(icon("terminal").size(px(14.)).with_animation(
                "terminal-pulse",
                Animation::new(Duration::from_secs(2)).repeat(),
                |icon, progress| icon.opacity(terminal_pulse_opacity(progress)),
            ))
            .into_any_element(),
    )
}

/// The ⌘1…⌘9 hint, centred on the row's right edge.
fn jump_badge(label: String, row_height: f32) -> Div {
    div()
        .absolute()
        .right(px(6.))
        .top(px((row_height - 20.) / 2.))
        .h_5()
        .flex()
        .items_center()
        .px_1p5()
        .rounded_full()
        .border_1()
        .border_color(tint("border", 0.8))
        .bg(tint("canvas", 0.95))
        .shadow_sm()
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .font_family("Menlo")
        .text_color(color("text"))
        .child(label)
}

impl Desktop {
    fn row_click(
        &mut self,
        thread_id: &str,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.modifiers();
        if modifiers.secondary() {
            self.sidebar.selection.toggle(thread_id);
            self.refresh_views(cx);
        } else if modifiers.shift {
            let ordered = self.views.sidebar.thread_ids();
            self.sidebar.selection.extend_to(thread_id, &ordered);
            self.refresh_views(cx);
        } else if event.click_count() >= 2 {
            self.start_rename(thread_id.to_owned(), MenuSurface::Sidebar, window, cx);
        } else {
            self.open_sidebar_thread(thread_id.to_owned(), cx);
        }
    }

    /// The thread menu, or the bulk menu when the row is part of a selection.
    fn row_menu(
        &mut self,
        thread_id: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = &self.sidebar.selection.selected;
        if selected.contains(&thread_id) {
            let items = self.views.sidebar.multi_select_menu(selected);
            let ids: Vec<_> = self
                .views
                .sidebar
                .rows()
                .filter(|row| selected.contains(&row.id))
                .map(|row| row.id.clone())
                .collect();
            if !items.is_empty() {
                self.open_selection_menu(items, ids, position, window, cx);
                return;
            }
        }
        self.open_thread_menu(thread_id, MenuSurface::Sidebar, position, window, cx);
    }

    /// The trailing slot: the status at rest, the row actions while hovered.
    /// Woke stays, since dismissing it is itself an action.
    fn render_trailing(
        &self,
        row: &SidebarThreadRow,
        hovered: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let slim = row.variant == SidebarRowVariant::Slim;
        let id = row.id.clone();
        let woke = matches!(
            row.trailing,
            SidebarRowTrailing::Status {
                status: SidebarTopStatus::Woke,
                ..
            }
        );
        let status = match &row.trailing {
            SidebarRowTrailing::Status {
                status,
                label,
                duration,
            } => {
                let (glyph, tone) = status_style(*status);
                let size = if slim { 12. } else { 16. };
                let body = h_flex()
                    .gap_1()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .when_some(glyph, |body, glyph| {
                        body.child(icon(glyph).size(px(size)).flex_shrink_0())
                    })
                    .child(label.clone())
                    .when_some(duration.clone(), |body, duration| body.child(duration));
                match (*status, row.woke_at) {
                    (SidebarTopStatus::Woke, Some(at)) => div().child(
                        body.id(element_id("woke", &id))
                            .cursor_pointer()
                            .when(slim, |body| body.text_color(color("warningForeground")))
                            .tooltip(|window, cx| {
                                Tooltip::new("Dismiss Woke notification").build(window, cx)
                            })
                            .on_click(cx.listener(move |view, _, _, cx| {
                                cx.stop_propagation();
                                view.perform(Intent::Thread {
                                    thread_id: id.clone(),
                                    action: ThreadAction::Visit { at },
                                });
                            })),
                    ),
                    _ => div().child(body),
                }
            }
            SidebarRowTrailing::WakesIn { label } => div()
                .text_color(color("updateForeground"))
                .child(label.clone()),
            SidebarRowTrailing::Time { label } => div()
                .text_color(if row.section == SidebarSection::Settled {
                    tint("secondaryLabel", 0.7)
                } else {
                    color("secondaryLabel")
                })
                .child(label.clone()),
        };
        let actions = row.actions.iter().map(|action| {
            let id = row.id.clone();
            let key = row.id.clone();
            match action {
                SidebarRowAction::DiscardDraft => {
                    row_button(element_id("discard", &key), "Discard draft")
                        .child(icon("x").size_3p5())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.stop_propagation();
                            view.perform(Intent::DiscardDraft {
                                draft_key: id.clone(),
                            });
                        }))
                }
                SidebarRowAction::Snooze => row_button(element_id("snooze", &key), "Snooze thread")
                    .child(icon("clock").size_3())
                    .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        view.open_snooze_menu(id.clone(), event.position(), window, cx);
                    })),
                SidebarRowAction::Settle => row_button(element_id("settle", &key), "Settle thread")
                    .when(slim, |button| button.child(icon("check").size_3()))
                    .when(!slim, |button| {
                        button.child(icon("check").size_3p5()).child("Settle")
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        cx.stop_propagation();
                        view.park_threads(
                            vec![id.clone()],
                            ThreadAction::Settle,
                            MenuSurface::Sidebar,
                            window,
                            cx,
                        );
                    })),
                SidebarRowAction::Unsettle => {
                    row_button(element_id("unsettle", &key), "Un-settle thread")
                        .child(icon("undo-2").size_3p5())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.stop_propagation();
                            view.perform(Intent::Thread {
                                thread_id: id.clone(),
                                action: ThreadAction::Unsettle,
                            });
                        }))
                }
                SidebarRowAction::Wake => row_button(element_id("wake", &key), "Wake thread now")
                    .child(icon("alarm-clock-off").size_3())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        cx.stop_propagation();
                        view.perform(Intent::Thread {
                            thread_id: id.clone(),
                            action: ThreadAction::Unsnooze,
                        });
                    })),
            }
        });
        h_flex()
            .ml_auto()
            .h(px(if slim { 24. } else { 20. }))
            .min_w_8()
            .flex_shrink_0()
            .justify_end()
            .items_center()
            .text_xs()
            .when(!hovered || woke, |slot| slot.child(status))
            .when(hovered, |slot| slot.children(actions))
    }

    /// The pin marker; it unpins when clicked.
    fn render_pin(&self, row: &SidebarThreadRow, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !row.pinned {
            return None;
        }
        let id = row.id.clone();
        Some(
            div()
                .id(element_id("unpin", &row.id))
                .group("pin")
                .flex_shrink_0()
                .cursor_pointer()
                .text_color(tint("textMuted", 0.65))
                .hover(|pin| pin.text_color(color("text")))
                .child(
                    div()
                        .group_hover("pin", |pin| pin.hidden())
                        .child(icon("pin").size_3()),
                )
                .child(
                    div()
                        .hidden()
                        .group_hover("pin", |pin| pin.block())
                        .child(icon("pin-off").size_3()),
                )
                .tooltip(|window, cx| Tooltip::new("Unpin thread").build(window, cx))
                .on_click(cx.listener(move |view, _, window, cx| {
                    cx.stop_propagation();
                    view.toggle_pin(&id, window, cx);
                }))
                .into_any_element(),
        )
    }

    fn toggle_pin(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let menu = self.snapshot.thread_menu(
            thread_id.to_owned(),
            ui::now_ms(),
            agent_core::view::thread_menu::ThreadMenuOptions::default(),
        );
        if let Some(item) = menu.and_then(|menu| {
            menu.items
                .into_iter()
                .find(|item| item.id == agent_core::view::thread_menu::ThreadMenuItemId::Unpin)
        }) && let Some(action) = item.action
        {
            self.run_thread_action(
                thread_id.to_owned(),
                MenuSurface::Sidebar,
                action,
                item.confirmation,
                window,
                cx,
            );
        }
    }

    /// Earlier owners small and faint, the current provider last.
    fn render_provider_stack(&self, row: &SidebarThreadRow, catalog: &ModelCatalog) -> Option<Div> {
        let current = catalog.instance(row.provider_stack.last()?)?;
        let earlier = row.provider_stack[..row.provider_stack.len() - 1]
            .iter()
            .filter_map(|id| catalog.instance(id))
            .map(|instance| {
                driver_icon(instance.driver)
                    .size_3()
                    .text_color(color("text"))
                    .opacity(0.35)
            });
        Some(
            h_flex()
                .ml_auto()
                .flex_shrink_0()
                .gap_1p5()
                .children(earlier)
                .child(
                    driver_icon(current.driver)
                        .size_3p5()
                        .text_color(color("text"))
                        .opacity(0.6),
                ),
        )
    }

    pub(super) fn render_thread_row(
        &mut self,
        row: &SidebarThreadRow,
        jump: Option<String>,
        catalog: &ModelCatalog,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id.clone();
        let hovered = self.sidebar.hovered.as_deref() == Some(id.as_str());
        let slim = row.variant == SidebarRowVariant::Slim;
        let renaming =
            self.menus.rename.as_ref().is_some_and(|rename| {
                rename.surface == MenuSurface::Sidebar && rename.thread_id == id
            });
        let lifted = self
            .sidebar
            .drag
            .as_ref()
            .is_some_and(|drag| drag.active == id);
        let (background, hover_background) = match row.surface {
            SidebarRowSurface::Active => (Some(color("sidebarRowActive")), None),
            SidebarRowSurface::Selected => (Some(color("sidebarRowSelected")), None),
            SidebarRowSurface::Draft => (Some(tint("warning", 0.04)), Some(tint("warning", 0.08))),
            SidebarRowSurface::Receded | SidebarRowSurface::Plain => {
                (None, Some(color("sidebarRowHover")))
            }
        };
        let foreground = if row.surface == SidebarRowSurface::Receded && !hovered {
            tint("sidebarMutedForeground", 0.75)
        } else {
            color("sidebarForeground")
        };
        let rename = if renaming {
            self.render_rename_input(cx)
        } else {
            None
        };
        let title = match rename {
            Some(input) => input,
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(if row.recede {
                    FontWeight::NORMAL
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(title_color(row.title_tone, slim && hovered))
                .when(row.title_regenerating, |title| title.opacity(0.55))
                .child(row.title.clone())
                .into_any_element(),
        };
        let pen = row.has_unsent_draft.then(|| {
            div()
                .id(element_id("draft-pen", &id))
                .flex_shrink_0()
                .child(
                    icon("square-pen")
                        .size_3()
                        .text_color(color("warningForeground")),
                )
                .tooltip(|window, cx| Tooltip::new("Unsent draft").build(window, cx))
        });
        let project = row.project_name.clone().unwrap_or_default();
        let project_image = self.project_icon_image(&row.project_id);
        let badge = div()
            .flex_shrink_0()
            .child(project_mark(project_image.clone(), &project, 16.));
        let terminal = || terminal_indicator(&id, row.terminal_processes.clone());
        let body = if slim {
            h_flex()
                .h_9()
                .px_2p5()
                .gap_2p5()
                .child(badge.when(
                    !hovered && (!row.active || row.section == SidebarSection::Settled),
                    |badge| badge.opacity(0.4),
                ))
                .children(pen)
                .child(title)
                .children(self.render_pin(row, cx))
                .children(terminal())
                .child(self.render_trailing(row, hovered, cx))
        } else {
            let branch = row.branch.clone().map(|branch| {
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .when_some(row.worktree_path.clone(), |line, path| {
                        let tip = format!("Worktree: {path} ({branch})");
                        line.child(
                            div()
                                .id(element_id("worktree", &id))
                                .flex_shrink_0()
                                .child(
                                    icon("folder-git-2")
                                        .size_3()
                                        .text_color(tint("textMuted", 0.4)),
                                )
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tip.clone()).build(window, cx)
                                }),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(tint("textMuted", 0.4))
                            .child(branch.clone()),
                    )
            });
            v_flex()
                .h(px(78.))
                .px(px(10.))
                .py_2()
                .child(
                    h_flex()
                        .h_5()
                        .min_w_0()
                        .gap_1p5()
                        .children(pen)
                        .child(badge)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(color("secondaryLabel"))
                                .font_weight(if row.recede {
                                    FontWeight::NORMAL
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .child(project),
                        )
                        .children(self.render_pin(row, cx))
                        .child(self.render_trailing(row, hovered, cx)),
                )
                .child(h_flex().mt_1().min_w_0().child(title))
                .child(
                    h_flex()
                        .mt_0p5()
                        .min_w_0()
                        .gap_1p5()
                        .text_xs()
                        .text_color(color("secondaryLabel"))
                        .child(match branch {
                            Some(branch) => branch,
                            None => h_flex().flex_1(),
                        })
                        .children(terminal())
                        .children(self.render_provider_stack(row, catalog)),
                )
        };
        let desktop = cx.entity().downgrade();
        let drag = ThreadDrag {
            id: id.clone(),
            section: row.section,
            title: row.title.clone(),
        };
        let click_id = id.clone();
        let menu_id = id.clone();
        let hover_id = id.clone();
        let surface = div()
            .id(element_id("row", &id))
            .relative()
            .w_full()
            .rounded(px(8.))
            .overflow_hidden()
            .cursor_pointer()
            .text_color(foreground)
            .when_some(background, |surface, background| surface.bg(background))
            .when_some(hover_background, |surface, hover| {
                surface.hover(move |surface| surface.bg(hover))
            })
            .when(lifted, |surface| surface.opacity(0.5))
            .when(self.sidebar.drag.is_none() && !renaming, |surface| {
                surface.tooltip(hover_card(row, catalog, project_image.clone()))
            })
            .when(row.faded && !hovered, |surface| surface.opacity(0.7))
            .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                let current = view.sidebar.hovered.as_deref() == Some(hover_id.as_str());
                if *hovered && !current {
                    view.sidebar.hovered = Some(hover_id.clone());
                    cx.notify();
                } else if !*hovered && current {
                    view.sidebar.hovered = None;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                view.row_click(&click_id, event, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                    view.row_menu(menu_id.clone(), event.position, window, cx)
                }),
            )
            .when(row.draggable && !renaming, |surface| {
                surface.on_drag(drag, move |drag, _, _, cx| {
                    let preview = cx.new(|_| DragPreview::new(drag.title.clone()));
                    let _ = desktop.update(cx, |view, cx| {
                        view.start_thread_drag(drag, preview.clone(), cx)
                    });
                    preview
                })
            })
            .child(body)
            .children(jump.map(|label| jump_badge(label, if slim { 36. } else { 78. })));
        let slot = div()
            .w_full()
            .when(!slim, |slot| slot.py_0p5())
            .child(surface);
        self.drop_slot(slot.id(element_id("slot", &id)), id, cx)
            .into_any_element()
    }

    pub(super) fn render_draft_row(
        &mut self,
        draft: &SidebarDraftRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = draft.draft_key.clone();
        let hovered = self.sidebar.hovered.as_deref() == Some(draft.draft_key.as_str());
        let project = draft.project_name.clone().unwrap_or_default();
        let open = draft.project_id.clone();
        let (menu_key, menu_project) = (draft.draft_key.clone(), draft.project_id.clone());
        let discard = draft.draft_key.clone();
        let hover_key = draft.draft_key.clone();
        div()
            .py_0p5()
            .child(
                div()
                    .id(element_id("draft-row", &key))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .bg(tint("warning", 0.04))
                    .hover(|row| row.bg(tint("warning", 0.08)))
                    .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                        view.sidebar.hovered = hovered.then(|| hover_key.clone());
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.clear_selection(cx);
                        view.new_thread(Some(open.clone()), cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                            view.open_draft_menu(
                                menu_key.clone(),
                                menu_project.clone(),
                                event.position,
                                window,
                                cx,
                            )
                        }),
                    )
                    .child(
                        v_flex()
                            .h(px(78.))
                            .px(px(10.))
                            .py_2()
                            .child(
                                h_flex()
                                    .h_5()
                                    .min_w_0()
                                    .gap_1p5()
                                    .child(
                                        icon("square-pen")
                                            .size_3()
                                            .text_color(color("warningForeground")),
                                    )
                                    .child(self.project_icon(&draft.project_id, &project, 16.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(color("secondaryLabel"))
                                            .child(project),
                                    )
                                    .when(hovered, |line| {
                                        line.child(
                                            row_button(
                                                element_id("discard-draft", &key),
                                                "Discard draft",
                                            )
                                            .px_1()
                                            .child(icon("x").size_3())
                                            .on_click(
                                                cx.listener(move |view, _, _, cx| {
                                                    cx.stop_propagation();
                                                    view.perform(Intent::DiscardDraft {
                                                        draft_key: discard.clone(),
                                                    });
                                                }),
                                            ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .mt_0p5()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tint("text", 0.9))
                                    .child(draft.preview.clone()),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_search_result(
        &mut self,
        index: usize,
        result: &SidebarSearchResult,
        query: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = self.sidebar.search_index == index || result.active;
        let id = result.id.clone();
        let snippet = result.snippet.clone().map(|snippet| {
            let (who, tone) = match result.snippet_source {
                Some(SidebarMatchSource::User) => ("You:", color("updateForeground")),
                _ => ("Agent:", color("successForeground")),
            };
            let parts = highlight_parts(&snippet, query);
            div()
                .truncate()
                .text_xs()
                .text_color(tint("textMuted", 0.85))
                .child(
                    h_flex()
                        .min_w_0()
                        .child(div().text_color(tone).child(who))
                        .child(" ")
                        .children(parts.into_iter().map(|part| {
                            div()
                                .when(part.highlighted, |text| {
                                    text.font_weight(FontWeight::SEMIBOLD)
                                        .text_color(color("text"))
                                })
                                .child(part.text)
                        })),
                )
        });
        h_flex()
            .id(("search-result", index))
            .min_h_9()
            .w_full()
            .px_2p5()
            .py_1()
            .gap_2p5()
            .rounded(px(8.))
            .cursor_pointer()
            .text_sm()
            .when(highlighted, |row| {
                row.bg(color("sidebarRowActive"))
                    .text_color(color("sidebarForeground"))
            })
            .when(!highlighted, |row| {
                row.text_color(tint("sidebarMutedForeground", 0.75))
                    .hover(|row| {
                        row.bg(color("sidebarRowHover"))
                            .text_color(color("sidebarForeground"))
                    })
            })
            .on_mouse_move(cx.listener(move |view, _, _, cx| {
                if view.sidebar.search_index != index {
                    view.sidebar.search_index = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |view, _, window, cx| {
                view.clear_search(window, cx);
                view.open_sidebar_thread(id.clone(), cx);
            }))
            .child(self.project_icon(
                &result.project_id,
                result.project_name.as_deref().unwrap_or_default(),
                16.,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2p5()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(result.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(tint("textMuted", 0.55))
                                    .child(result.time_label.clone()),
                            ),
                    )
                    .children(snippet),
            )
            .into_any_element()
    }

    /// The pinned header and divider and the empty-section hints: nothing at
    /// rest, drop slots while a row is lifted.
    pub(super) fn render_boundary(
        &mut self,
        marker: SidebarListMarker,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // At rest a boundary takes no room, the list gap included.
        let Some(drag) = &self.sidebar.drag else {
            return div().h_0().mb(px(-1.)).into_any_element();
        };
        let (section, label) = match marker {
            SidebarListMarker::PinnedHeader => (SidebarSection::Pinned, "Pinned"),
            SidebarListMarker::PinnedDivider | SidebarListMarker::ActivePlaceholder => {
                (SidebarSection::Active, "Active")
            }
            _ => (SidebarSection::Settled, "Settled"),
        };
        let target = drag.target == Some(section);
        let slot = match marker {
            SidebarListMarker::PinnedHeader | SidebarListMarker::PinnedDivider => h_flex()
                .h_6()
                .px_2()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if target {
                            color("accent")
                        } else {
                            tint("sidebarForeground", 0.8)
                        })
                        .child(label),
                )
                .child(div().h_px().flex_1().bg(if target {
                    tint("accent", 0.5)
                } else {
                    tint("sidebarForeground", 0.25)
                })),
            _ => {
                let rows = self
                    .views
                    .sidebar
                    .rows()
                    .filter(|row| row.section == section)
                    .count();
                let hint = rows == 0
                    || (drag.from == section
                        && rows == 1
                        && drag.target.is_some_and(|target| target != section));
                if !hint {
                    return self
                        .drop_slot(
                            div()
                                .id(ElementId::Name(sidebar_marker_id(marker).into()))
                                .h_1(),
                            sidebar_marker_id(marker),
                            cx,
                        )
                        .into_any_element();
                }
                h_flex()
                    .mx_0p5()
                    .h_9()
                    .justify_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_dashed()
                    .text_xs()
                    .when(target, |hint| {
                        hint.border_color(tint("accent", 0.4))
                            .bg(tint("accent", 0.05))
                            .text_color(color("accent"))
                    })
                    .when(!target, |hint| {
                        hint.border_color(tint("sidebarForeground", 0.25))
                            .text_color(tint("sidebarForeground", 0.8))
                    })
                    .child(label)
            }
        };
        self.drop_slot(
            slot.id(ElementId::Name(sidebar_marker_id(marker).into())),
            sidebar_marker_id(marker),
            cx,
        )
        .into_any_element()
    }

    pub(super) fn render_shelf(
        &mut self,
        header: &SidebarShelfHeader,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section = header.section;
        let marker = match section {
            SidebarSection::Working => SidebarListMarker::WorkingHeader,
            SidebarSection::Snoozed => SidebarListMarker::SnoozedHeader,
            _ => SidebarListMarker::SettledHeader,
        };
        let drag = self.sidebar.drag.as_ref();
        let target = drag.is_some_and(|drag| drag.target == Some(section));
        let dragging = drag.is_some() && section == SidebarSection::Settled;
        let (label, line) = if target {
            (color("accent"), tint("accent", 0.5))
        } else if dragging {
            (
                tint("sidebarForeground", 0.8),
                tint("sidebarForeground", 0.25),
            )
        } else if section == SidebarSection::Snoozed {
            (color("updateForeground"), tint("updateForeground", 0.2))
        } else {
            (
                tint("sidebarMutedForeground", 0.6),
                tint("sidebarBorder", 0.6),
            )
        };
        let chevron = icon("chevron-down").size_3().flex_shrink_0();
        let shelf = h_flex()
            .id(ElementId::Name(sidebar_marker_id(marker).into()))
            .mx_0p5()
            .h_8()
            .flex_shrink_0()
            .gap_2()
            .px_2()
            .rounded(px(8.))
            .cursor_pointer()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(label)
            .when(header.bottom_anchor, |shelf| shelf.mt_auto())
            .on_click(cx.listener(move |view, _, _, cx| {
                let options = &mut view.sidebar.options;
                let expanded = match section {
                    SidebarSection::Working => &mut options.working_expanded,
                    SidebarSection::Snoozed => &mut options.snoozed_expanded,
                    _ => &mut options.settled_expanded,
                };
                *expanded = !*expanded;
                view.refresh_views(cx);
            }))
            .child(div().flex_shrink_0().child(header.label.clone()))
            .child(div().h_px().min_w_2().flex_1().bg(line))
            .child(if header.expanded {
                chevron.rotate(radians(PI))
            } else {
                chevron
            });
        self.drop_slot(shelf, sidebar_marker_id(marker), cx)
            .into_any_element()
    }

    pub(super) fn render_show_more(&mut self, count: u32, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id("show-more-settled")
            .h_9()
            .w_full()
            .gap_2p5()
            .px_2p5()
            .rounded(px(8.))
            .cursor_pointer()
            .text_sm()
            .text_color(tint("sidebarMutedForeground", 0.55))
            .hover(|button| {
                button
                    .bg(color("sidebarRowHover"))
                    .text_color(color("sidebarForeground"))
            })
            .on_click(cx.listener(|view, _, _, cx| {
                view.sidebar.options.settled_pages += 1;
                view.refresh_views(cx);
            }))
            .child(icon("plus").size_4())
            .child(format!("Show {count} more"))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::terminal_pulse_opacity;
    use core::prelude::v1::test;

    #[test]
    fn the_terminal_icon_holds_full_then_half_and_steps_between() {
        assert_eq!(terminal_pulse_opacity(0.), 1.);
        assert_eq!(terminal_pulse_opacity(0.39), 1.);
        assert!((terminal_pulse_opacity(0.425) - 11. / 12.).abs() < 1e-4);
        assert_eq!(terminal_pulse_opacity(0.6), 0.5);
        assert!((terminal_pulse_opacity(0.925) - 7. / 12.).abs() < 1e-4);
    }
}
