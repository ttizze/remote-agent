//! The composer footer's draft controls: the model picker, traits, runtime
//! mode and the Build/Plan toggle, and the new-thread Host picker.
use super::{Desktop, on_click, outline};
use crate::app::{
    hosts::Hosts,
    ui::{color, driver_icon, icon, tint},
};
use agent_core::{
    state::Intent,
    view::{
        composer::view::ComposerView,
        models::{
            ProviderInstance,
            picker::{
                LegacyModelsSection, ModelPickerRow, ModelPickerView, PickerRail, PickerRailItem,
            },
            traits::{SpeedIcon, TraitControl},
        },
        new_thread::NewThreadWorkspaceView,
        projects::selection::ThreadWorkspaceMode,
    },
};
use agent_domain::{InteractionMode, RuntimeMode};
use gpui_kit::{
    component::{
        Disableable, Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::{DropdownMenu, PopupMenuItem},
        notification::Notification,
        popover::Popover,
        switch::Switch,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::rc::Rc;

pub(super) struct PickerState {
    pub(super) open: bool,
    query: Entity<InputState>,
    rail: Option<PickerRail>,
    /// Instances whose "Legacy models" row was toggled since the picker opened.
    toggled_legacy: Vec<String>,
}
impl PickerState {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Desktop>,
        subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
        subscriptions.push(cx.subscribe(&query, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        Self {
            open: false,
            query,
            rail: None,
            toggled_legacy: vec![],
        }
    }
}

/// The new-thread branch picker: open, its search field, and the project
/// whose branches were last asked for.
pub(super) struct BranchPickerState {
    open: bool,
    query: Entity<InputState>,
    requested: Option<Option<String>>,
}
impl BranchPickerState {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Desktop>,
        subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        subscriptions.push(cx.subscribe(&query, |view, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().trim().to_owned();
                view.perform(Intent::SearchNewThreadBranches { query });
                cx.notify();
            }
        }));
        Self {
            open: false,
            query,
            requested: None,
        }
    }
}

/// A 24 px control of the strip under the new-thread composer.
fn strip_control(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .h(px(24.))
        .px(px(7.))
        .gap_1()
        .text_xs()
        .font_weight(FontWeight::NORMAL)
        .text_color(color("textMuted").opacity(0.7))
}

/// The runtime mode's icon.
fn runtime_mode_icon(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "lock",
        RuntimeMode::AutoAcceptEdits => "pen-line",
        RuntimeMode::Auto => "sparkles",
        RuntimeMode::FullAccess => "lock-open",
    }
}

/// A 28px footer control.
fn control(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .px(px(10.))
        .gap(px(6.))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color("textMuted"))
}

/// The 16px rule between footer controls.
fn separator() -> Div {
    div()
        .w(px(1.))
        .h(px(16.))
        .mx(px(2.))
        .flex_none()
        .bg(color("border"))
}

/// An `#rrggbb` accent color.
fn accent_color(hex: &str) -> Option<Hsla> {
    let value = u32::from_str_radix(hex.strip_prefix('#')?, 16).ok()?;
    Some(rgb(value).into())
}

/// A provider instance's mark, with its account badge when it has one: the
/// accent color with white initials, else the card color.
fn instance_icon(instance: &ProviderInstance, size: f32) -> Div {
    let accent = instance.accent_color.as_deref().and_then(accent_color);
    let badge = if accent.is_some() { 12. } else { 14. };
    div()
        .relative()
        .flex_none()
        .size(px(size + 4.))
        .flex()
        .items_center()
        .justify_center()
        .child(driver_icon(instance.driver).size(px(size)))
        .when(instance.show_badge && size >= 16., |mark| {
            mark.child(
                div()
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .h(px(badge))
                    .min_w(px(badge))
                    .px(px(2.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(color("muted"))
                    .shadow_sm()
                    .text_size(px(if accent.is_some() { 7. } else { 8. }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .map(|badge| match accent {
                        Some(accent) => badge.bg(accent).text_color(hsla(0., 0., 1., 1.)),
                        None => badge.bg(color("surface")).text_color(color("textMuted")),
                    })
                    .child(instance.initials.clone()),
            )
        })
}

impl Desktop {
    pub(crate) fn toggle_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_model_picker(!self.composer.picker.open, window, cx);
    }

    fn set_model_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.picker.open = open;
        if !open {
            self.composer.picker.rail = None;
            self.composer.picker.toggled_legacy.clear();
            self.composer
                .picker
                .query
                .update(cx, |query, cx| query.set_value("", window, cx));
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// The model picker, traits, runtime mode and Build/Plan toggle.
    pub(super) fn composer_controls(
        &mut self,
        composer: &ComposerView,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let controls = &composer.controls;
        let traits = composer.traits.visible.then(|| {
            let traits = composer.traits.clone();
            let speed = traits.trigger.speed_icon.map(|speed| {
                let mark = icon("zap").size(px(16.)).text_color(color("text"));
                match speed {
                    SpeedIcon::Fast => h_flex().child(mark),
                    SpeedIcon::Ultrafast => h_flex().child(mark).child(
                        icon("zap")
                            .size(px(16.))
                            .text_color(color("text"))
                            .ml(px(-8.)),
                    ),
                }
            });
            let view = view.clone();
            control("composer-traits")
                .child(
                    h_flex().gap(px(6.)).children(speed).child(
                        div()
                            .max_w(px(192.))
                            .truncate()
                            .child(traits.trigger.label.clone()),
                    ),
                )
                .dropdown_caret(true)
                .tooltip(traits.accessible_label.clone())
                .accessibility_label(traits.accessible_label.clone())
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                    for (index, control) in traits.controls.iter().enumerate() {
                        if index > 0 {
                            menu = menu.separator();
                        }
                        match control {
                            TraitControl::Select {
                                id,
                                label,
                                choices,
                                selected,
                                note,
                                disabled,
                            } => {
                                menu = menu.label(label.clone());
                                for choice in choices {
                                    let (descriptor_id, value) = (id.clone(), choice.id.clone());
                                    let (name, default, description) = (
                                        choice.label.clone(),
                                        choice.is_default,
                                        choice.description.clone(),
                                    );
                                    menu = menu.item(
                                        PopupMenuItem::element(move |_, _| {
                                            v_flex()
                                                .gap(px(2.))
                                                .child(h_flex().gap_1().child(name.clone()).when(
                                                    default,
                                                    |row| {
                                                        row.child(
                                                            div()
                                                                .rounded_sm()
                                                                .border_1()
                                                                .border_color(color("border"))
                                                                .px_1()
                                                                .text_size(px(10.))
                                                                .child("Default"),
                                                        )
                                                    },
                                                ))
                                                .when_some(description.clone(), |column, text| {
                                                    column.child(
                                                        div()
                                                            .max_w(px(224.))
                                                            .text_xs()
                                                            .text_color(
                                                                color("textMuted").opacity(0.8),
                                                            )
                                                            .child(text),
                                                    )
                                                })
                                        })
                                        .checked(*selected == choice.id)
                                        .disabled(*disabled)
                                        .on_click(
                                            on_click(&view, move |view, _, _| {
                                                view.perform(Intent::SelectTrait {
                                                    descriptor_id: descriptor_id.clone(),
                                                    choice: value.clone(),
                                                })
                                            }),
                                        ),
                                    );
                                }
                                if let Some(note) = note {
                                    menu = menu.label(note.clone());
                                }
                            }
                            TraitControl::Toggle { id, label, on } => {
                                menu = menu.label(label.clone());
                                for (text, value) in [("On", true), ("Off", false)] {
                                    let descriptor_id = id.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(text).checked(*on == value).on_click(
                                            on_click(&view, move |view, _, _| {
                                                view.perform(Intent::ToggleTrait {
                                                    descriptor_id: descriptor_id.clone(),
                                                    on: value,
                                                })
                                            }),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    menu
                })
        });
        let mode = &controls.runtime_mode;
        let choices = controls.runtime_mode_choices.clone();
        let current = mode.mode;
        let runtime = {
            let view = view.clone();
            control("composer-runtime-mode")
                .icon(icon(runtime_mode_icon(mode.mode)).size(px(16.)))
                .label(mode.label.clone())
                .dropdown_caret(true)
                .tooltip(mode.description.clone())
                .accessibility_label("Runtime mode")
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                    for choice in &choices {
                        let (label, description, mode) = (
                            choice.label.clone(),
                            choice.description.clone(),
                            choice.mode,
                        );
                        menu = menu.item(
                            PopupMenuItem::element(move |_, _| {
                                v_flex()
                                    .min_w(px(256.))
                                    .gap(px(2.))
                                    .child(
                                        h_flex()
                                            .gap(px(6.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(
                                                icon(runtime_mode_icon(mode))
                                                    .size(px(14.))
                                                    .text_color(color("textMuted")),
                                            )
                                            .child(label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .line_height(px(16.))
                                            .text_color(color("textMuted"))
                                            .child(description.clone()),
                                    )
                            })
                            .checked(mode == current)
                            .on_click(on_click(&view, move |view, _, _| {
                                view.perform(Intent::SetRuntimeMode { mode })
                            })),
                        );
                    }
                    menu
                })
        };
        let toggle = controls.interaction_toggle.clone().map(|toggle| {
            let plan = toggle.mode == InteractionMode::Plan;
            let next = toggle.toggled;
            control("composer-interaction-mode")
                .icon(
                    icon(if plan { "pencil-ruler" } else { "bot" }).size(px(if plan {
                        16.
                    } else {
                        18.
                    })),
                )
                .label(toggle.label.clone())
                .selected(plan)
                .tooltip(toggle.tooltip.clone())
                .accessibility_label(toggle.tooltip.clone())
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.perform(Intent::SetInteractionMode { mode: next })
                }))
        });
        h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .child(self.composer_model_picker(composer, cx))
            .when_some(traits, |row, traits| row.child(separator()).child(traits))
            .child(separator())
            .child(runtime)
            .when_some(toggle, |row, toggle| row.child(separator()).child(toggle))
            .into_any_element()
    }

    fn composer_model_picker(
        &mut self,
        composer: &ComposerView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let trigger = &composer.model_trigger;
        let open = self.composer.picker.open;
        let picker = open.then(|| {
            let query = self.composer.picker.query.read(cx).value().to_string();
            Rc::new(self.snapshot.model_picker(
                query,
                self.composer.picker.rail.clone(),
                self.composer.picker.toggled_legacy.clone(),
            ))
        });
        let query = self.composer.picker.query.clone();
        let focus = query.read(cx).focus_handle(cx);
        let button = control("composer-model-picker")
            .ml(px(-10.))
            .when_some(trigger.instance.as_ref(), |button, instance| {
                button.child(instance_icon(instance, 16.))
            })
            .child(
                div()
                    .max_w(px(200.))
                    .truncate()
                    .child(trigger.label.clone()),
            )
            .dropdown_caret(true)
            .accessibility_label(trigger.label.clone());
        let on_open = view.clone();
        Popover::new("composer-model-popover")
            .anchor(Anchor::BottomLeft)
            .open(open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = on_open.update(cx, |view, cx| view.set_model_picker(open, window, cx));
            })
            .track_focus(&focus)
            .p_0()
            .trigger(button)
            .content(move |_, _, _| match &picker {
                Some(picker) => model_picker_content(picker, &query, &view).into_any_element(),
                None => div().into_any_element(),
            })
            .into_any_element()
    }

    /// The strip under the draft's composer: the Host the new thread runs
    /// on, its workspace and its branch.
    pub(super) fn composer_host_strip(
        &mut self,
        workspace: Option<&NewThreadWorkspaceView>,
        project_id: Option<&String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let requested = Some(project_id.cloned());
        if self.composer.branches.requested != requested {
            self.composer.branches.requested = requested;
            if workspace.is_some() {
                self.perform(Intent::SearchNewThreadBranches {
                    query: String::new(),
                });
            }
        }
        let name = self.snapshot.host_name.as_deref().unwrap_or("Local");
        h_flex()
            .mx(px(22.))
            .pt_1()
            .pb_1()
            .pl_1()
            .pr_2()
            .gap_1()
            .rounded_b(px(16.))
            .border_1()
            .border_t_0()
            .border_color(outline())
            .text_xs()
            .text_color(color("textMuted").opacity(0.7))
            .child(Hosts::menu(
                &self.hosts,
                "composer-host",
                self.remote.as_ref().map(|remote| remote.id.as_str()),
                name,
                self.connecting,
                cx,
            ))
            .when_some(workspace, |strip, workspace| {
                strip
                    .child(div().mx(px(2.)).h(px(14.)).w(px(1.)).bg(color("border")))
                    .child(self.workspace_select(workspace, cx))
                    .child(div().flex_1())
                    .child(self.branch_select(workspace, window, cx))
            })
            .into_any_element()
    }

    /// "Current checkout" or "New worktree".
    fn workspace_select(
        &mut self,
        workspace: &NewThreadWorkspaceView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let mode = workspace.mode;
        let local_worktree = mode == ThreadWorkspaceMode::Local && workspace.in_worktree;
        let local_label = if mode == ThreadWorkspaceMode::Local {
            workspace.workspace_label.clone()
        } else {
            "Current checkout".into()
        };
        let local_icon = if local_worktree {
            "folder-git"
        } else {
            "folder"
        };
        let trigger_icon = match mode {
            ThreadWorkspaceMode::Worktree => "folder-git-2",
            ThreadWorkspaceMode::Local => local_icon,
        };
        strip_control("new-thread-workspace")
            .icon(icon(trigger_icon).size(px(12.)))
            .label(workspace.workspace_label.clone())
            .dropdown_caret(true)
            .accessibility_label("Workspace")
            .tooltip(workspace.workspace_label.clone())
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                let local = view.clone();
                let worktree = view.clone();
                menu.label("Workspace")
                    .item(
                        PopupMenuItem::new(local_label.clone())
                            .icon(icon(local_icon))
                            .checked(mode == ThreadWorkspaceMode::Local)
                            .on_click(on_click(&local, |view, _, _| {
                                view.perform(Intent::SetNewThreadWorkspace {
                                    mode: ThreadWorkspaceMode::Local,
                                })
                            })),
                    )
                    .item(
                        PopupMenuItem::new("New worktree")
                            .icon(icon("folder-git-2"))
                            .checked(mode == ThreadWorkspaceMode::Worktree)
                            .on_click(on_click(&worktree, |view, _, _| {
                                view.perform(Intent::SetNewThreadWorkspace {
                                    mode: ThreadWorkspaceMode::Worktree,
                                })
                            })),
                    )
            })
            .into_any_element()
    }

    fn set_branch_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.branches.open = open;
        if open {
            self.perform(Intent::SearchNewThreadBranches {
                query: String::new(),
            });
        } else {
            self.composer
                .branches
                .query
                .update(cx, |query, cx| query.set_value("", window, cx));
        }
        cx.notify();
    }

    /// The branch the new thread works on, or the base of its worktree.
    fn branch_select(
        &mut self,
        workspace: &NewThreadWorkspaceView,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let query = self.composer.branches.query.clone();
        let focus = query.read(cx).focus_handle(cx);
        let trigger = strip_control("new-thread-branch")
            .disabled(workspace.branches_loading && workspace.branches.is_empty())
            .icon(icon("git-branch").size(px(12.)).opacity(0.7))
            .child(
                div()
                    .max_w(px(240.))
                    .truncate()
                    .child(workspace.branch_label.clone()),
            )
            .dropdown_caret(true)
            .accessibility_label(workspace.branch_role.clone());
        let workspace = workspace.clone();
        let on_open = view.clone();
        Popover::new("new-thread-branch-popover")
            .anchor(Anchor::BottomRight)
            .open(self.composer.branches.open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = on_open.update(cx, |view, cx| view.set_branch_picker(open, window, cx));
            })
            .track_focus(&focus)
            .p_0()
            .trigger(trigger)
            .content(move |_, _, _| branch_list(&workspace, &query, &view).into_any_element())
            .into_any_element()
    }
}

/// The branch picker's popup: search, the branches, and "Start from origin"
/// while choosing a worktree's base.
fn branch_list(
    workspace: &NewThreadWorkspaceView,
    query: &Entity<InputState>,
    view: &WeakEntity<Desktop>,
) -> Div {
    let rows = workspace.branches.iter().map(|branch| {
        let (name, worktree_path) = (branch.name.clone(), branch.worktree_path.clone());
        h_flex()
            .id(SharedString::from(format!("branch-{}", branch.name)))
            .w_full()
            .min_h_7()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .when(branch.selected, |row| row.bg(color("text").opacity(0.08)))
            .hover(|row| row.bg(color("accentSurface")))
            .on_click(on_click(view, move |view, window, cx| {
                view.perform_then(
                    Intent::SelectNewThreadBranch {
                        branch: name.clone(),
                        worktree_path: worktree_path.clone(),
                    },
                    |_, result, window, cx| {
                        if let Err(error) = result {
                            window.push_notification(
                                Notification::error(
                                    agent_core::presentation::error::error_message(error),
                                )
                                .title("Could not switch branch"),
                                cx,
                            );
                        }
                    },
                );
                view.set_branch_picker(false, window, cx);
                view.focus_composer(window, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(branch.name.clone()),
            )
            .children(branch.badge.clone().map(|badge| {
                div()
                    .flex_shrink_0()
                    .text_size(px(10.))
                    .text_color(color("textMuted").opacity(0.45))
                    .child(badge)
            }))
    });
    let status = if workspace.branches_loading {
        Some("Loading refs...".to_owned())
    } else {
        workspace.branch_error.clone()
    };
    let list = if workspace.branches.is_empty() {
        div()
            .p_2()
            .text_center()
            .text_sm()
            .text_color(color("textMuted"))
            .child(status.clone().unwrap_or_else(|| "No refs found.".into()))
            .into_any_element()
    } else {
        v_flex()
            .id("branch-list")
            .max_h(px(224.))
            .overflow_y_scroll()
            .pl_1()
            .pt_2()
            .pb_1()
            .children(rows)
            .into_any_element()
    };
    let origin = (workspace.mode == ThreadWorkspaceMode::Worktree).then(|| {
        let toggle = view.clone();
        let on = workspace.start_from_origin;
        h_flex()
            .id("start-from-origin")
            .justify_between()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(tint("border", 0.6))
            .text_xs()
            .tooltip(|window, cx| {
                Tooltip::new(
                    "Creates the worktree from the latest matching branch on origin instead of your local branch.",
                )
                .build(window, cx)
            })
            .child(
                h_flex()
                    .gap_1p5()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color("textMuted"))
                    .child(icon("refresh-cw").size(px(12.)))
                    .child("Start from origin"),
            )
            .child(
                Switch::new("start-from-origin-switch")
                    .small()
                    .checked(on)
                    .accessibility_label("Start worktree from origin")
                    .on_click(move |on: &bool, _, cx| {
                        let on = *on;
                        let _ = toggle.update(cx, |view, _| {
                            view.perform(Intent::SetNewThreadStartFromOrigin { on })
                        });
                    }),
            )
    });
    v_flex()
        .w(px(320.))
        .child(
            div().px_3().pt(px(10.)).child(
                div()
                    .pb(px(6.))
                    .border_b_1()
                    .border_color(tint("border", 0.7))
                    .child(
                        Input::new(query).appearance(false).small().prefix(
                            icon("search")
                                .size(px(16.))
                                .text_color(color("textMuted").opacity(0.55)),
                        ),
                    ),
            ),
        )
        .child(list)
        .children(
            status
                .filter(|_| !workspace.branches.is_empty())
                .map(|status| {
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(status)
                }),
        )
        .children(origin)
}

/// The picker: the provider rail, the search field and the model rows.
fn model_picker_content(
    picker: &ModelPickerView,
    query: &Entity<InputState>,
    view: &WeakEntity<Desktop>,
) -> Div {
    let rail = (!picker.rail.is_empty()).then(|| {
        v_flex()
            .id("model-picker-rail")
            .w(px(44.))
            .flex_none()
            .overflow_y_scroll()
            .bg(tint("muted", 0.3))
            .p_1()
            .gap_1()
            .children(
                picker
                    .rail
                    .iter()
                    .enumerate()
                    .map(|(index, item)| rail_item(index, item, view)),
            )
    });
    let mut rows = picker
        .rows
        .iter()
        .map(|row| model_row(row, view))
        .collect::<Vec<_>>();
    if let Some(legacy) = &picker.legacy {
        let at = (legacy.current_count as usize).min(rows.len());
        rows.insert(at, legacy_row(legacy, view));
    }
    h_flex()
        .w(px(360.))
        .max_h(px(346.))
        .overflow_hidden()
        .items_start()
        .children(rail)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .max_h(px(346.))
                .bg(tint("muted", 0.4))
                .when(!picker.rail.is_empty(), |main| {
                    main.border_l_1().border_color(color("border").opacity(0.7))
                })
                .child(
                    div()
                        .p_1()
                        .border_b_1()
                        .border_color(color("border").opacity(0.7))
                        .child(
                            Input::new(query).appearance(false).small().prefix(
                                icon("search").size(px(14.)).text_color(color("textMuted")),
                            ),
                        ),
                )
                .child(
                    v_flex()
                        .id("model-picker-rows")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .py(px(6.))
                        .pl_2()
                        .pr(px(1.))
                        .gap(px(1.))
                        .children(rows),
                )
                .when_some(picker.empty_label.clone(), |main, label| {
                    main.child(
                        div()
                            .p_4()
                            .text_center()
                            .text_xs()
                            .text_color(color("textMuted"))
                            .child(label),
                    )
                }),
        )
}

/// The collapsible "Legacy models" row after an instance's current models.
fn legacy_row(legacy: &LegacyModelsSection, view: &WeakEntity<Desktop>) -> AnyElement {
    let instance = legacy.instance_id.clone();
    h_flex()
        .id(SharedString::from(format!("model-row-{}", legacy.key)))
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .rounded_sm()
        .px_2()
        .py_1()
        .cursor_pointer()
        .hover(|item| item.bg(color("accentSurface")))
        .on_click(on_click(view, move |view, _, cx| {
            let toggled = &mut view.composer.picker.toggled_legacy;
            match toggled.iter().position(|id| *id == instance) {
                Some(index) => {
                    toggled.remove(index);
                }
                None => toggled.push(instance.clone()),
            }
            cx.notify();
        }))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(legacy.label.clone()),
                )
                .child(
                    div()
                        .mt_1()
                        .truncate()
                        .text_xs()
                        .text_color(color("textMuted").opacity(0.7))
                        .child(legacy.detail.clone()),
                ),
        )
        .child(
            icon(if legacy.expanded {
                "chevron-down"
            } else {
                "chevron-right"
            })
            .size(px(16.))
            .text_color(color("textMuted")),
        )
        .into_any_element()
}

fn rail_item(index: usize, item: &PickerRailItem, view: &WeakEntity<Desktop>) -> AnyElement {
    let rail = item.rail.clone();
    let tooltip = item.tooltip.clone();
    let favorites = matches!(item.rail, PickerRail::Favorites);
    div()
        .w_full()
        .when(favorites, |cell| {
            cell.pb_1()
                .border_b_1()
                .border_color(color("border").opacity(0.7))
        })
        .child(
            div()
                .id(("model-rail", index))
                .relative()
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .when(item.selected, |cell| {
                    cell.child(
                        div()
                            .absolute()
                            .right(px(-4.))
                            .top(px(8.))
                            .h(px(20.))
                            .w(px(3.))
                            .rounded_l_full()
                            .bg(color("accent")),
                    )
                })
                .when(item.disabled, |cell| cell.opacity(0.5))
                .when(!item.disabled, |cell| {
                    cell.cursor_pointer()
                        .hover(|cell| cell.bg(color("text").opacity(0.1)))
                        .on_click(on_click(view, move |view, _, cx| {
                            view.composer.picker.rail = Some(rail.clone());
                            cx.notify();
                        }))
                })
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(match &item.instance {
                    Some(instance) => instance_icon(instance, 20.).into_any_element(),
                    None => icon("star")
                        .size(px(20.))
                        .text_color(color("warning"))
                        .into_any_element(),
                }),
        )
        .into_any_element()
}

fn model_row(row: &ModelPickerRow, view: &WeakEntity<Desktop>) -> AnyElement {
    let (instance_id, driver, slug) = (row.instance_id.clone(), row.driver, row.slug.clone());
    let (favorite_instance, favorite_model) = (row.instance_id.clone(), row.slug.clone());
    let disabled = row.disabled_reason.clone();
    let favorite_label = if row.favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    h_flex()
        .id(SharedString::from(format!("model-row-{}", row.key)))
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .rounded_sm()
        .px_2()
        .py_1()
        .when(row.selected, |item| item.bg(color("text").opacity(0.08)))
        .map(|item| match disabled.clone() {
            Some(reason) => item
                .opacity(0.64)
                .cursor_not_allowed()
                .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx)),
            None => item
                .cursor_pointer()
                .hover(|item| item.bg(color("accentSurface")))
                .on_click(on_click(view, move |view, window, cx| {
                    view.perform(Intent::SetModel {
                        instance_id: instance_id.clone(),
                        driver,
                        model: slug.clone(),
                        options: vec![],
                    });
                    view.set_model_picker(false, window, cx);
                })),
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(row.name.clone()),
                )
                .child(
                    h_flex()
                        .mt_1()
                        .gap(px(6.))
                        .child(driver_icon(row.driver).size(px(12.)))
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(color("textMuted").opacity(0.7))
                                .child(row.provider_name.clone()),
                        ),
                ),
        )
        .child(
            Button::new(SharedString::from(format!("favorite-{}", row.key)))
                .icon(
                    icon("star")
                        .size(px(12.))
                        .text_color(color(if row.favorite { "warning" } else { "textMuted" })),
                )
                .ghost()
                .xsmall()
                .disabled(row.disabled_reason.is_some())
                .tooltip(favorite_label)
                .accessibility_label(favorite_label)
                .on_click({
                    let view = view.clone();
                    move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = view.update(cx, |view, _| {
                            view.perform(Intent::ToggleFavoriteModel {
                                instance_id: favorite_instance.clone(),
                                model: favorite_model.clone(),
                            })
                        });
                    }
                }),
        )
        .into_any_element()
}
