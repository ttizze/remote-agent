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
                parse_model_picker_legacy_section_key,
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
        menu::{DropdownMenu, PopupMenu, PopupMenuItem},
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
    /// The row the arrow keys and pointer last highlighted; `None` follows
    /// the list's own first choice.
    highlighted: Option<String>,
    /// The provider rail item keyboard focus is on, by index.
    rail_focus: Option<usize>,
    rows_scroll: ScrollHandle,
}
impl PickerState {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Desktop>,
        subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search models..."));
        subscriptions.push(cx.subscribe(&query, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.composer.picker.highlighted = None;
                cx.notify();
            }
        }));
        Self {
            open: false,
            query,
            rail: None,
            toggled_legacy: vec![],
            highlighted: None,
            rail_focus: None,
            rows_scroll: ScrollHandle::new(),
        }
    }
}

/// What the picker's keyboard state shows: the highlighted row and the
/// focused rail item.
#[derive(Clone, Default)]
struct PickerFocus {
    highlighted: Option<String>,
    rail_focus: Option<usize>,
    /// The jump shortcut label of each model that has one, by key.
    jump_labels: Vec<(String, String)>,
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

const MODEL_PICKER_MAX_LABEL_WIDTH: f32 = 200.;
const MODEL_PICKER_MIN_LABEL_WIDTH: f32 = 48.;

fn model_picker_label_width(window: &Window, label: &str) -> f32 {
    let style = window.text_style();
    let run = TextRun {
        len: label.len(),
        font: font(style.font_family.clone()),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(
        window
            .text_system()
            .shape_line(label.into(), px(14.), &[run], None)
            .width,
    )
}

fn model_picker_minimum_width(natural_width: f32, label_width: f32) -> f32 {
    ((natural_width - label_width.min(MODEL_PICKER_MAX_LABEL_WIDTH)).max(0.)
        + MODEL_PICKER_MIN_LABEL_WIDTH)
        .min(natural_width)
}

fn estimated_model_picker_widths(window: &Window, trigger: &ModelPickerTrigger) -> (f32, f32) {
    // The button has 20px of horizontal padding, a 16px caret and a 4px
    // content gap. The instance mark is 20px wide and adds one more gap; the
    // trigger keeps its existing -10px leading margin.
    let fixed = 20. + 16. + 4. + if trigger.instance.is_some() { 24. } else { 0. } - 10.;
    let label = model_picker_label_width(window, &trigger.label);
    let natural = fixed + label.min(MODEL_PICKER_MAX_LABEL_WIDTH);
    let minimum = (fixed + MODEL_PICKER_MIN_LABEL_WIDTH).min(natural);
    (natural, minimum)
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
        self.composer.picker.highlighted = None;
        self.composer.picker.rail_focus = None;
        if open {
            let query = self.composer.picker.query.clone();
            query.update(cx, |input, cx| input.focus(window, cx));
        } else {
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

    /// Whether the model picker is open, for the keymap's `modelPickerOpen`.
    pub(crate) fn model_picker_open(&self) -> bool {
        self.composer.picker.open
    }

    /// The open picker as it shows now, filtered by `query`.
    fn current_model_picker(&self, query: String) -> ModelPickerView {
        self.snapshot.model_picker(
            query,
            self.composer.picker.rail.clone(),
            self.composer.picker.toggled_legacy.clone(),
        )
    }

    /// The row the keyboard acts on: the one last highlighted while it is
    /// listed, else the first match of a search or the selected model.
    fn picker_highlight(&self, picker: &ModelPickerView, query: &str) -> Option<String> {
        let keys = picker.navigable_keys();
        self.composer
            .picker
            .highlighted
            .clone()
            .filter(|key| keys.contains(key))
            .or_else(|| {
                if query.trim().is_empty() {
                    picker.initial_highlight()
                } else {
                    keys.first().cloned()
                }
            })
    }

    /// Selects a model row, or opens or folds the "Legacy models" row.
    fn choose_picker_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(instance) = parse_model_picker_legacy_section_key(key) {
            self.toggle_legacy_models(instance.to_owned(), cx);
            return;
        }
        let query = self.composer.picker.query.read(cx).value().to_string();
        let picker = self.current_model_picker(query);
        let Some(row) = picker
            .rows
            .iter()
            .find(|row| row.key == key && row.disabled_reason.is_none())
        else {
            return;
        };
        self.perform(Intent::SetModel {
            instance_id: row.instance_id.clone(),
            driver: row.driver,
            model: row.slug.clone(),
            options: vec![],
        });
        self.set_model_picker(false, window, cx);
    }

    fn toggle_legacy_models(&mut self, instance: String, cx: &mut Context<Self>) {
        let toggled = &mut self.composer.picker.toggled_legacy;
        match toggled.iter().position(|id| *id == instance) {
            Some(index) => {
                toggled.remove(index);
            }
            None => toggled.push(instance),
        }
        cx.notify();
    }

    /// The provider shortcuts: the previous or next rail item, clearing the
    /// search.
    pub(crate) fn step_model_picker_provider(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.composer.picker.open {
            return false;
        }
        let rail = self
            .current_model_picker(String::new())
            .adjacent_rail(forward);
        self.composer
            .picker
            .query
            .update(cx, |query, cx| query.set_value("", window, cx));
        self.composer.picker.rail = Some(rail);
        self.composer.picker.highlighted = None;
        cx.notify();
        true
    }

    /// A jump shortcut: selects the `index`th selectable model listed.
    pub(crate) fn jump_model_picker(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.composer.picker.open {
            return false;
        }
        let query = self.composer.picker.query.read(cx).value().to_string();
        let picker = self.current_model_picker(query);
        if let Some(key) = picker.jump_targets().get(index).map(|row| row.key.clone()) {
            self.choose_picker_key(&key, window, cx);
        }
        true
    }

    /// The picker's own keys: arrows move the highlight (or through the
    /// provider rail), Enter chooses, Escape closes, Left or Shift+Tab from
    /// an empty search moves to the rail and Right back to the search.
    fn model_picker_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let plain = !modifiers.platform && !modifiers.control && !modifiers.alt;
        let query = self.composer.picker.query.read(cx).value().to_string();
        let picker = self.current_model_picker(query.clone());
        if keystroke.key == "escape" {
            self.set_model_picker(false, window, cx);
            return true;
        }
        if let Some(focus) = self.composer.picker.rail_focus {
            let enabled: Vec<usize> = picker
                .rail
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.disabled)
                .map(|(index, _)| index)
                .collect();
            let position = enabled.iter().position(|index| *index == focus);
            match keystroke.key.as_str() {
                "up" | "down" if plain && !modifiers.shift && !enabled.is_empty() => {
                    let count = enabled.len();
                    let next = match (position, keystroke.key == "down") {
                        (None, _) => 0,
                        (Some(at), true) => (at + 1) % count,
                        (Some(at), false) => (at + count - 1) % count,
                    };
                    self.composer.picker.rail_focus = Some(enabled[next]);
                }
                "right" | "tab" if plain && !modifiers.shift => {
                    self.composer.picker.rail_focus = None;
                }
                "enter" | "space" if plain => {
                    if let Some(item) = picker.rail.get(focus).filter(|item| !item.disabled) {
                        self.composer.picker.rail = Some(item.rail.clone());
                        self.composer.picker.highlighted = None;
                    }
                }
                _ => return false,
            }
            cx.notify();
            return true;
        }
        match keystroke.key.as_str() {
            "up" | "down" if plain && !modifiers.shift => {
                let current = self.picker_highlight(&picker, &query);
                let next = picker.step_highlight(current.as_deref(), keystroke.key == "down");
                if let Some(next) = &next {
                    self.scroll_picker_to(&picker, next);
                }
                self.composer.picker.highlighted = next;
            }
            "enter" if plain => {
                if let Some(key) = self.picker_highlight(&picker, &query) {
                    self.choose_picker_key(&key, window, cx);
                }
            }
            "left" if plain && !modifiers.shift && query.is_empty() && !picker.rail.is_empty() => {
                self.focus_picker_rail(&picker);
            }
            "tab" if plain && modifiers.shift && !picker.rail.is_empty() => {
                self.focus_picker_rail(&picker);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Moves keyboard focus to the selected rail item, else the first one
    /// that can be chosen.
    fn focus_picker_rail(&mut self, picker: &ModelPickerView) {
        self.composer.picker.rail_focus = picker
            .rail
            .iter()
            .position(|item| item.selected && !item.disabled)
            .or_else(|| picker.rail.iter().position(|item| !item.disabled));
    }

    /// Keeps the highlighted row in view.
    fn scroll_picker_to(&self, picker: &ModelPickerView, key: &str) {
        let mut children: Vec<&str> = picker.rows.iter().map(|row| row.key.as_str()).collect();
        if let Some(legacy) = &picker.legacy {
            let at = (legacy.current_count as usize).min(children.len());
            children.insert(at, legacy.key.as_str());
        }
        if let Some(index) = children.iter().position(|child| *child == key) {
            self.composer.picker.rows_scroll.scroll_to_item(index);
        }
    }

    /// The model picker, traits, runtime mode and Build/Plan toggle.
    pub(super) fn composer_controls(
        &mut self,
        composer: &ComposerView,
        window: &mut Window,
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
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                    traits_menu(menu, &traits, &view)
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
        // The blocks that move into "More composer controls" from the end
        // when the footer runs out of room.
        let has_traits = traits.is_some();
        let has_toggle = toggle.is_some();
        let mut blocks: Vec<AnyElement> = vec![];
        if let Some(traits) = traits {
            blocks.push(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .child(separator())
                    .child(traits)
                    .into_any_element(),
            );
        }
        blocks.push(
            h_flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(separator())
                .child(runtime)
                .when_some(toggle, |block, toggle| {
                    block.child(separator()).child(toggle)
                })
                .into_any_element(),
        );
        let block_count = blocks.len();
        let layout = self.composer.footer_layout;
        let hidden = layout.hidden_count.min(block_count);
        let picker_compact = hidden == block_count;
        let picker_label = composer.model_trigger.label.clone();
        let picker_measurement_key =
            format!("{}:{picker_label}", composer.model_trigger.instance_id);
        let picker_label_width = model_picker_label_width(window, &picker_label);
        let estimated_picker_widths =
            estimated_model_picker_widths(window, &composer.model_trigger);
        let traits_hidden = has_traits && hidden >= block_count;
        let mode_hidden = hidden >= 1;
        let overflow =
            self.composer_overflow_menu(composer, traits_hidden, mode_hidden && has_toggle, cx);
        let widths: Rc<std::cell::RefCell<Vec<f32>>> = Rc::default();
        let measured = widths.clone();
        let owner = view.clone();
        let row = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .when(!layout.visible, |row| row.invisible())
            .on_children_prepainted(move |bounds, _, _| {
                *measured.borrow_mut() = bounds
                    .iter()
                    .map(|bounds| f32::from(bounds.size.width))
                    .collect();
            })
            .child(
                div()
                    .when(!picker_compact, |picker| picker.flex_none())
                    .when(picker_compact, |picker| {
                        picker.flex_1().min_w_0().overflow_hidden()
                    })
                    .child(self.composer_model_picker(composer, picker_compact, cx)),
            )
            .children(blocks.into_iter().enumerate().map(|(index, block)| {
                let hidden_block = index >= block_count - hidden;
                div()
                    .flex_none()
                    .when(hidden_block, |block| block.invisible().absolute())
                    .child(block)
            }))
            .child(
                div()
                    .flex_none()
                    .when(hidden == 0, |menu| menu.invisible().absolute())
                    .child(overflow),
            );
        div()
            .flex_1()
            .min_w_0()
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(host) = bounds.first().map(|bounds| f32::from(bounds.size.width)) else {
                    return;
                };
                let widths = widths.borrow();
                // The picker, each block, then the overflow trigger.
                if widths.len() != block_count + 2 {
                    return;
                }
                let widths = widths.clone();
                let _ = owner.update(cx, |view, cx| {
                    let (natural_fixed_width, minimum_fixed_width) = if picker_compact {
                        view.composer
                            .footer_picker_measurement
                            .as_ref()
                            .filter(|(key, _, _)| key == &picker_measurement_key)
                            .map(|(_, natural, minimum)| (*natural, *minimum))
                            .unwrap_or(estimated_picker_widths)
                    } else {
                        let natural = widths[0];
                        let minimum = model_picker_minimum_width(natural, picker_label_width);
                        (natural, minimum)
                    };
                    view.composer.footer_picker_measurement = Some((
                        picker_measurement_key.clone(),
                        natural_fixed_width,
                        minimum_fixed_width,
                    ));
                    let measurement =
                        agent_core::view::composer::footer_layout::FooterMeasurement {
                            gap: 4.,
                            natural_fixed_width,
                            minimum_fixed_width,
                            block_widths: widths[1..=block_count].to_vec(),
                            overflow_width: widths[block_count + 1],
                        };
                    let previous = view.composer.footer_layout;
                    let next = agent_core::view::composer::footer_layout::resolve_footer_layout(
                        &measurement,
                        host,
                        Some(previous),
                    );
                    if next != previous {
                        view.composer.footer_layout = next;
                        cx.notify();
                    }
                });
            })
            .child(row)
            .into_any_element()
    }

    /// "More composer controls": the traits, mode and access that no longer
    /// fit in the footer.
    fn composer_overflow_menu(
        &self,
        composer: &ComposerView,
        traits_hidden: bool,
        mode_hidden: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let traits = traits_hidden.then(|| composer.traits.clone());
        let toggle = composer
            .controls
            .interaction_toggle
            .clone()
            .filter(|_| mode_hidden);
        let choices = composer.controls.runtime_mode_choices.clone();
        let current = composer.controls.runtime_mode.mode;
        control("composer-overflow")
            .px(px(6.))
            .icon(icon("ellipsis").size(px(16.)))
            .accessibility_label("More composer controls")
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                if let Some(traits) = &traits {
                    menu = traits_menu(menu, traits, &view).separator();
                }
                if let Some(toggle) = &toggle {
                    menu = menu.label("Mode");
                    for (label, mode) in [
                        ("Chat", InteractionMode::Default),
                        ("Plan", InteractionMode::Plan),
                    ] {
                        let current = toggle.mode;
                        menu =
                            menu.item(PopupMenuItem::new(label).checked(current == mode).on_click(
                                on_click(&view, move |view, _, _| {
                                    if mode != current {
                                        view.perform(Intent::SetInteractionMode { mode })
                                    }
                                }),
                            ));
                    }
                    menu = menu.separator();
                }
                menu = menu.label("Access");
                for choice in &choices {
                    let mode = choice.mode;
                    menu = menu.item(
                        PopupMenuItem::new(choice.label.clone())
                            .checked(mode == current)
                            .on_click(on_click(&view, move |view, _, _| {
                                if mode != current {
                                    view.perform(Intent::SetRuntimeMode { mode })
                                }
                            })),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn composer_model_picker(
        &mut self,
        composer: &ComposerView,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let trigger = &composer.model_trigger;
        let open = self.composer.picker.open;
        let picker = open.then(|| {
            let query = self.composer.picker.query.read(cx).value().to_string();
            let picker = self.current_model_picker(query.clone());
            let keymap = self.snapshot.keymap(crate::app::keymap::MAC);
            let context = agent_core::view::keybindings::KeyContext {
                terminal_open: self.header_panels().terminal_open,
                ..Default::default()
            };
            let focus = PickerFocus {
                highlighted: self.picker_highlight(&picker, &query),
                rail_focus: self.composer.picker.rail_focus,
                jump_labels: picker
                    .jump_targets()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, row)| {
                        Some((row.key.clone(), keymap.model_jump_label(index, &context)?))
                    })
                    .collect(),
            };
            Rc::new((picker, focus, self.composer.picker.rows_scroll.clone()))
        });
        let query = self.composer.picker.query.clone();
        let focus = query.read(cx).focus_handle(cx);
        let button = control("composer-model-picker")
            .max_w_full()
            .ml(px(-10.))
            .when_some(trigger.instance.as_ref(), |button, instance| {
                button.child(instance_icon(instance, 16.))
            })
            .child(
                div()
                    .min_w_0()
                    .max_w(px(MODEL_PICKER_MAX_LABEL_WIDTH))
                    .when(compact, |label| label.max_w_full())
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
                Some(picker) => {
                    let (picker, focus, scroll) = picker.as_ref();
                    let keys = view.clone();
                    model_picker_content(picker, focus, scroll, &query, &view)
                        .capture_key_down(move |event, window, cx| {
                            let handled = keys
                                .update(cx, |view, cx| view.model_picker_key(event, window, cx))
                                .unwrap_or(false);
                            if handled {
                                cx.stop_propagation();
                            }
                        })
                        .into_any_element()
                }
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
            .tooltip(if workspace.workspace_path.is_empty() {
                workspace.workspace_label.clone()
            } else {
                workspace.workspace_path.clone()
            })
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
            .disabled(workspace.branches_loading)
            .icon(icon("git-branch").size(px(12.)).opacity(0.7))
            .child(
                div()
                    .max_w(px(240.))
                    .truncate()
                    .child(workspace.desktop_branch_label.clone()),
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

/// The strip under a started thread's composer: its workspace, which cannot
/// change any more, and its branch.
pub(super) fn thread_context_strip(
    workspace: &agent_core::view::header::WorkspaceRow,
    branch: Option<&str>,
) -> AnyElement {
    let tooltip: SharedString = workspace
        .path
        .clone()
        .unwrap_or_else(|| workspace.label.clone())
        .into();
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
        .child(
            h_flex()
                .id("thread-context-workspace")
                .h(px(24.))
                .px(px(7.))
                .gap_1()
                .min_w_0()
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    icon(if workspace.in_worktree {
                        "folder-git"
                    } else {
                        "folder"
                    })
                    .size(px(12.)),
                )
                .child(div().min_w_0().truncate().child(workspace.label.clone())),
        )
        .child(div().flex_1())
        .children(branch.map(|branch| {
            h_flex()
                .h(px(24.))
                .px(px(7.))
                .gap_1()
                .min_w_0()
                .child(icon("git-branch").size(px(12.)).opacity(0.7))
                .child(div().max_w(px(240.)).truncate().child(branch.to_owned()))
        }))
        .into_any_element()
}

/// The model's traits as menu sections: each select's choices with their
/// Default badge and description, and each toggle's On and Off.
fn traits_menu(
    mut menu: PopupMenu,
    traits: &agent_core::view::models::traits::TraitsView,
    view: &WeakEntity<Desktop>,
) -> PopupMenu {
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
                                .child(h_flex().gap_1().child(name.clone()).when(default, |row| {
                                    row.child(
                                        div()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(color("border"))
                                            .px_1()
                                            .text_size(px(10.))
                                            .child("Default"),
                                    )
                                }))
                                .when_some(description.clone(), |column, text| {
                                    column.child(
                                        div()
                                            .max_w(px(224.))
                                            .text_xs()
                                            .text_color(color("textMuted").opacity(0.8))
                                            .child(text),
                                    )
                                })
                        })
                        .checked(*selected == choice.id)
                        .disabled(*disabled)
                        .on_click(on_click(view, move |view, _, _| {
                            view.perform(Intent::SelectTrait {
                                descriptor_id: descriptor_id.clone(),
                                choice: value.clone(),
                            })
                        })),
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
                    menu = menu.item(PopupMenuItem::new(text).checked(*on == value).on_click(
                        on_click(view, move |view, _, _| {
                            view.perform(Intent::ToggleTrait {
                                descriptor_id: descriptor_id.clone(),
                                on: value,
                            })
                        }),
                    ));
                }
            }
        }
    }
    menu
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
    let create = workspace.create_ref.clone().map(|create| {
        let name = create.name.clone();
        h_flex()
            .id("branch-create-ref")
            .w_full()
            .min_h_7()
            .px_2()
            .py_1()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .hover(|row| row.bg(color("accentSurface")))
            .on_click(on_click(view, move |view, window, cx| {
                view.perform_then(
                    Intent::CreateNewThreadBranch { name: name.clone() },
                    |_, result, window, cx| {
                        if let Err(error) = result {
                            window.push_notification(
                                Notification::error(
                                    agent_core::presentation::error::error_message(error),
                                )
                                .title("Failed to create and switch ref."),
                                cx,
                            );
                        }
                    },
                );
                view.set_branch_picker(false, window, cx);
                view.focus_composer(window, cx);
            }))
            .child(div().min_w_0().truncate().child(create.label))
    });
    let rows = rows
        .map(IntoElement::into_any_element)
        .chain(create.map(IntoElement::into_any_element));
    let status = if workspace.branches_loading {
        Some("Loading refs...".to_owned())
    } else {
        workspace.branch_error.clone()
    };
    let list = if workspace.branches.is_empty() && workspace.create_ref.is_none() {
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
    focus: &PickerFocus,
    scroll: &ScrollHandle,
    query: &Entity<InputState>,
    view: &WeakEntity<Desktop>,
) -> Div {
    let rail =
        (!picker.rail.is_empty()).then(|| {
            v_flex()
                .id("model-picker-rail")
                .w(px(44.))
                .flex_none()
                .overflow_y_scroll()
                .bg(tint("muted", 0.3))
                .p_1()
                .gap_1()
                .children(picker.rail.iter().enumerate().map(|(index, item)| {
                    rail_item(index, item, focus.rail_focus == Some(index), view)
                }))
        });
    let highlighted = focus.highlighted.as_deref();
    let mut rows = picker
        .rows
        .iter()
        .map(|row| {
            let jump = focus
                .jump_labels
                .iter()
                .find(|(key, _)| *key == row.key)
                .map(|(_, label)| label.clone());
            model_row(row, highlighted == Some(row.key.as_str()), jump, view)
        })
        .collect::<Vec<_>>();
    if let Some(legacy) = &picker.legacy {
        let at = (legacy.current_count as usize).min(rows.len());
        rows.insert(
            at,
            legacy_row(legacy, highlighted == Some(legacy.key.as_str()), view),
        );
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
                        .track_scroll(scroll)
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
fn legacy_row(
    legacy: &LegacyModelsSection,
    highlighted: bool,
    view: &WeakEntity<Desktop>,
) -> AnyElement {
    let instance = legacy.instance_id.clone();
    let key = legacy.key.clone();
    let hover = view.clone();
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
        .when(highlighted, |item| item.bg(color("accentSurface")))
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                let key = key.clone();
                let _ = hover.update(cx, |view, cx| {
                    view.composer.picker.highlighted = Some(key);
                    cx.notify();
                });
            }
        })
        .on_click(on_click(view, move |view, _, cx| {
            view.toggle_legacy_models(instance.clone(), cx)
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

fn rail_item(
    index: usize,
    item: &PickerRailItem,
    focused: bool,
    view: &WeakEntity<Desktop>,
) -> AnyElement {
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
                .when(focused, |cell| cell.bg(color("text").opacity(0.1)))
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
                })
                .when(item.new_badge, |cell| {
                    cell.child(
                        div()
                            .absolute()
                            .right(px(-2.))
                            .top(px(2.))
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_color(color("updateForeground"))
                            .child(icon("sparkles").size(px(8.))),
                    )
                }),
        )
        .into_any_element()
}

fn model_row(
    row: &ModelPickerRow,
    highlighted: bool,
    jump: Option<String>,
    view: &WeakEntity<Desktop>,
) -> AnyElement {
    let (favorite_instance, favorite_model) = (row.instance_id.clone(), row.slug.clone());
    let disabled = row.disabled_reason.clone();
    let favorite_label = if row.favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    let key = row.key.clone();
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
        .when(highlighted, |item| item.bg(color("accentSurface")))
        .map(|item| match disabled.clone() {
            Some(reason) => item
                .opacity(0.64)
                .cursor_not_allowed()
                .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx)),
            None => {
                let hover = view.clone();
                let hovered_key = key.clone();
                item.cursor_pointer()
                    .on_hover(move |hovered, _, cx| {
                        if *hovered {
                            let key = hovered_key.clone();
                            let _ = hover.update(cx, |view, cx| {
                                view.composer.picker.highlighted = Some(key);
                                cx.notify();
                            });
                        }
                    })
                    .on_click(on_click(view, move |view, window, cx| {
                        view.choose_picker_key(&key, window, cx)
                    }))
            }
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .child(row.name.clone()),
                        )
                        .when(row.badge.as_deref() == Some("new"), |line| {
                            line.child(
                                div()
                                    .id(SharedString::from(format!("model-new-{}", row.key)))
                                    .flex_none()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(color("update").opacity(0.35))
                                    .bg(color("update").opacity(0.15))
                                    .px(px(2.))
                                    .text_size(px(10.))
                                    .line_height(px(10.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(color("updateForeground"))
                                    .tooltip(|window, cx| {
                                        Tooltip::new("New model").build(window, cx)
                                    })
                                    .child("NEW"),
                            )
                        }),
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
        .children(jump.map(|label| {
            div()
                .flex_none()
                .px_1()
                .rounded(px(4.))
                .border_1()
                .border_color(color("border"))
                .bg(color("muted"))
                .text_size(px(11.))
                .text_color(color("textMuted"))
                .child(label)
        }))
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

#[cfg(test)]
mod tests {
    use super::model_picker_minimum_width;

    #[test]
    fn model_picker_measurement_keeps_a_distinct_readable_minimum() {
        let minimum = model_picker_minimum_width(260., 200.);
        assert_eq!(minimum, 108.);
        assert!(minimum < 260.);
    }

    #[test]
    fn model_picker_minimum_does_not_exceed_a_short_label() {
        assert_eq!(model_picker_minimum_width(90., 24.), 90.);
    }
}
