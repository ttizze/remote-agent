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
            picker::{ModelPickerRow, ModelPickerView, PickerRail, PickerRailItem},
            traits::{SpeedIcon, TraitControl},
        },
    },
};
use agent_domain::{InteractionMode, RuntimeMode};
use gpui_kit::{
    component::{
        Disableable, Selectable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::{DropdownMenu, PopupMenuItem},
        popover::Popover,
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
        }
    }
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

/// A provider instance's mark, with its account badge when it has one.
fn instance_icon(instance: &ProviderInstance, size: f32) -> Div {
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(driver_icon(instance.driver).size(px(size)))
        .when(instance.show_badge, |mark| {
            mark.child(
                div()
                    .absolute()
                    .right(px(-4.))
                    .bottom(px(-3.))
                    .rounded(px(3.))
                    .px(px(2.))
                    .bg(color("surfaceOverlay"))
                    .border_1()
                    .border_color(color("border"))
                    .text_size(px(7.))
                    .font_weight(FontWeight::BOLD)
                    .child(instance.initials.clone()),
            )
        })
}

impl Desktop {
    pub(super) fn toggle_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_model_picker(!self.composer.picker.open, window, cx);
    }

    fn set_model_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.picker.open = open;
        if !open {
            self.composer.picker.rail = None;
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
            Rc::new(
                self.snapshot
                    .model_picker(query, self.composer.picker.rail.clone(), vec![]),
            )
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

    /// The Host the new thread runs on, under the draft's composer.
    pub(super) fn composer_host_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let name = self.snapshot.host_name.as_deref().unwrap_or("Local");
        h_flex()
            .mx(px(22.))
            .pt_1()
            .pb_1()
            .pl_1()
            .pr_2()
            .rounded_b(px(18.))
            .border_1()
            .border_t_0()
            .border_color(outline())
            .child(Hosts::menu(
                &self.hosts,
                "composer-host",
                self.remote.as_ref().map(|remote| remote.id.as_str()),
                name,
                self.connecting,
                cx,
            ))
            .into_any_element()
    }
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
    let rows = picker
        .rows
        .iter()
        .map(|row| model_row(row, view))
        .collect::<Vec<_>>();
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
                .size(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .when(item.selected, |cell| cell.bg(color("accentSurface")))
                .when(item.disabled, |cell| cell.opacity(0.5))
                .when(!item.disabled, |cell| {
                    cell.cursor_pointer()
                        .hover(|cell| cell.bg(color("accentSurface")))
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
        .rounded_md()
        .px_2()
        .py(px(6.))
        .when(row.selected, |item| item.bg(color("accentSurface")))
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
