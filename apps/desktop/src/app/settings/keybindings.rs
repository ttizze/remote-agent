//! The Keybindings page: the Host's keybindings, searchable, with recording
//! a new shortcut, adding a binding, resetting to defaults and opening
//! `keybindings.json`.
use super::{Choice, Row, page_container, section, select_sized};
use crate::app::{
    Desktop,
    keymap::{MAC, is_modifier_only, key_press},
    ui::{color, icon, text_2xs, tint},
};
use agent_core::{
    state::Intent,
    view::keybindings::{
        KeybindingRow, KeybindingSource, KeybindingTarget, command_label, key_caps,
        keybinding_command_options, keybinding_conflict_labels, keybinding_from_press,
        keybinding_rows,
    },
};
use gpui_kit::{
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::{DropdownMenu, PopupMenuItem},
        notification::Notification,
        tooltip::Tooltip,
    },
    prelude::FluentBuilder,
    *,
};

/// A shortcut being recorded: for an existing binding, or a new one.
struct Recording {
    target: Option<KeybindingRow>,
    command: Option<String>,
    key: String,
}

pub(super) struct KeybindingsState {
    search: Entity<InputState>,
    search_open: bool,
    recording: Option<Recording>,
    /// The command whose change the Host is saving.
    saving: Option<String>,
    capture: FocusHandle,
    _subscription: Subscription,
}

impl KeybindingsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search keybindings"));
        let subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            search,
            search_open: false,
            recording: None,
            saving: None,
            capture: cx.focus_handle(),
            _subscription: subscription,
        }
    }
    pub(super) fn recording(&self) -> bool {
        self.recording.is_some()
    }
}

/// A shortcut as key caps.
fn key_group(key: &str) -> Div {
    h_flex()
        .h_7()
        .gap_0p5()
        .items_center()
        .children(key_caps(key, MAC).into_iter().map(|cap| {
            div()
                .h_5()
                .min_w_5()
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(color("muted"))
                .text_xs()
                .text_color(color("textMuted"))
                .child(cap)
        }))
}

/// The warning beside a shortcut another command also runs.
fn conflict_warning(id: impl Into<ElementId>, labels: &[String]) -> Option<AnyElement> {
    if labels.is_empty() {
        return None;
    }
    let text: SharedString = format!("Also bound to {}", labels.join(", ")).into();
    Some(
        div()
            .id(id)
            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            .child(
                icon("triangle-alert")
                    .size(px(14.))
                    .text_color(color("warning")),
            )
            .into_any_element(),
    )
}

impl Desktop {
    fn start_recording(
        &mut self,
        target: Option<KeybindingRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = target.as_ref().map(|row| row.command.clone());
        self.settings.keybindings.recording = Some(Recording {
            target,
            command,
            key: String::new(),
        });
        window.focus(&self.settings.keybindings.capture, cx);
        cx.notify();
    }

    /// Asks the Host to bind a shortcut, replacing `replace`; a failure keeps
    /// the recording open and says why.
    fn save_keybinding(
        &mut self,
        rule: KeybindingTarget,
        replace: Option<KeybindingTarget>,
        cx: &mut Context<Self>,
    ) {
        self.settings.keybindings.saving = Some(rule.command.clone());
        cx.notify();
        self.perform_then(
            Intent::UpsertKeybinding { rule, replace },
            |view, result, window, cx| {
                view.settings.keybindings.saving = None;
                match result {
                    Ok(_) => view.settings.keybindings.recording = None,
                    Err(error) => window.push_notification(
                        Notification::error(agent_core::presentation::error::error_message(error))
                            .title("Unable to save keybinding"),
                        cx,
                    ),
                }
                cx.notify();
            },
        );
    }

    fn save_recording(&mut self, cx: &mut Context<Self>) {
        let Some(recording) = self.settings.keybindings.recording.as_ref() else {
            return;
        };
        let Some(command) = recording.command.clone() else {
            return;
        };
        if recording.key.trim().is_empty() {
            return;
        }
        let rule = KeybindingTarget {
            key: recording.key.trim().to_owned(),
            command,
            when: recording
                .target
                .as_ref()
                .map(|row| row.when.clone())
                .filter(|when| !when.trim().is_empty()),
        };
        let replace = recording
            .target
            .as_ref()
            .map(|row| KeybindingTarget::from(&row.rule));
        self.save_keybinding(rule, replace, cx);
    }

    /// Puts a changed default back to its command's default shortcut.
    fn reset_keybinding(&mut self, row: KeybindingRow, cx: &mut Context<Self>) {
        let Some(key) = row.default_key.clone() else {
            return;
        };
        let rule = KeybindingTarget {
            key,
            command: row.command.clone(),
            when: Some(row.default_when.clone()).filter(|when| !when.trim().is_empty()),
        };
        self.save_keybinding(rule, Some(KeybindingTarget::from(&row.rule)), cx);
    }

    fn remove_keybinding(&mut self, row: KeybindingRow, cx: &mut Context<Self>) {
        self.settings.keybindings.saving = Some(row.command.clone());
        cx.notify();
        self.perform_then(
            Intent::RemoveKeybinding {
                rule: KeybindingTarget::from(&row.rule),
            },
            |view, result, window, cx| {
                view.settings.keybindings.saving = None;
                if let Err(error) = result {
                    window.push_notification(
                        Notification::error(agent_core::presentation::error::error_message(error))
                            .title("Unable to remove keybinding"),
                        cx,
                    );
                }
                cx.notify();
            },
        );
    }

    /// Opens the Host's `keybindings.json` when the Host is this machine.
    fn keybindings_file(&self) -> Option<std::path::PathBuf> {
        if self.remote.is_some() {
            return None;
        }
        self.snapshot
            .keybindings
            .as_ref()
            .map(|config| std::path::PathBuf::from(&config.path))
    }

    pub(super) fn render_keybindings(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.settings.keybindings;
        let query = state.search.read(cx).value().to_string();
        let rules = self.snapshot.keybinding_rules();
        let all_rows = keybinding_rows(&rules, "");
        let shown = keybinding_rows(&rules, &query);
        let adding = state
            .recording
            .as_ref()
            .is_some_and(|recording| recording.target.is_none());
        let count = shown.len() + usize::from(adding);
        let mut rows: Vec<AnyElement> = vec![];
        if let Some(recording) = state
            .recording
            .as_ref()
            .filter(|recording| recording.target.is_none())
        {
            rows.push(self.new_binding_row(recording, &all_rows, &rules, cx));
        }
        if shown.is_empty() && !adding {
            rows.push(
                div()
                    .px_4()
                    .py_12()
                    .text_center()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("No keybindings match your search.")
                    .into_any_element(),
            );
        }
        for (index, row) in shown.into_iter().enumerate() {
            rows.push(self.binding_row(index, row, &all_rows, cx));
        }
        let search = if state.search_open {
            Input::new(&state.search)
                .small()
                .w(px(176.))
                .prefix(icon("search").size(px(14.)).text_color(color("textMuted")))
                .into_any_element()
        } else {
            h_flex()
                .gap(px(6.))
                .child(
                    text_2xs(div())
                        .text_color(color("textMuted"))
                        .child(format!(
                            "{count} {}",
                            if count == 1 { "binding" } else { "bindings" }
                        )),
                )
                .child(
                    Button::new("search-keybindings")
                        .icon(icon("search"))
                        .ghost()
                        .xsmall()
                        .tooltip("Search keybindings")
                        .accessibility_label("Search keybindings")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.settings.keybindings.search_open = true;
                            let search = view.settings.keybindings.search.clone();
                            search.update(cx, |input, cx| input.focus(window, cx));
                            cx.notify();
                        })),
                )
                .into_any_element()
        };
        let file = self.keybindings_file();
        let actions = h_flex()
            .gap(px(6.))
            .child(search)
            .child(
                Button::new("add-keybinding")
                    .icon(icon("plus"))
                    .ghost()
                    .xsmall()
                    .tooltip("Add keybinding")
                    .accessibility_label("Add keybinding")
                    .on_click(
                        cx.listener(|view, _, window, cx| view.start_recording(None, window, cx)),
                    ),
            )
            .child(
                Button::new("open-keybindings-file")
                    .icon(icon("file-braces"))
                    .ghost()
                    .xsmall()
                    .disabled(file.is_none())
                    .tooltip("Open keybindings.json")
                    .accessibility_label("Open keybindings.json")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(file) = &file {
                            cx.open_with_system(file);
                        }
                    })),
            )
            .into_any_element();
        let page = page_container(
            896.,
            vec![section(Some("Keybindings".into()), None, Some(actions), rows).into_any_element()],
        );
        div()
            .id("keybindings")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .track_focus(&self.settings.keybindings.capture)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.key == "f"
                    && keystroke.modifiers.secondary()
                    && !keystroke.modifiers.alt
                {
                    view.settings.keybindings.search_open = true;
                    let search = view.settings.keybindings.search.clone();
                    search.update(cx, |input, cx| input.focus(window, cx));
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                let Some(recording) = view.settings.keybindings.recording.as_mut() else {
                    if keystroke.key == "escape" && view.settings.keybindings.search_open {
                        view.settings.keybindings.search_open = false;
                        let search = view.settings.keybindings.search.clone();
                        search.update(cx, |input, cx| input.set_value("", window, cx));
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                };
                cx.stop_propagation();
                if keystroke.key == "escape" && !keystroke.modifiers.modified() {
                    view.settings.keybindings.recording = None;
                } else if !is_modifier_only(keystroke)
                    && let Some(key) = keybinding_from_press(&key_press(keystroke), MAC)
                {
                    recording.key = key;
                }
                cx.notify();
            }))
            .child(page)
            .into_any_element()
    }

    fn binding_row(
        &self,
        index: usize,
        row: KeybindingRow,
        all_rows: &[KeybindingRow],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let recording = self
            .settings
            .keybindings
            .recording
            .as_ref()
            .filter(|recording| {
                recording.target.as_ref().map(|target| &target.id) == Some(&row.id)
            });
        let saving = self.settings.keybindings.saving.as_deref() == Some(row.command.as_str());
        let conflicts = match recording.filter(|recording| !recording.key.is_empty()) {
            Some(recording) => {
                keybinding_conflict_labels(all_rows, &row.id, &recording.key, &row.when)
            }
            None => row.conflicts.clone(),
        };
        let group = SharedString::from(format!("keybinding-{index}"));
        let title = h_flex()
            .id(("keybinding-title", index))
            .gap_2()
            .child(row.title.clone())
            .when(row.source != KeybindingSource::Default, |title| {
                title.child(
                    text_2xs(div())
                        .px_1p5()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(tint("border", 0.6))
                        .text_color(color("textMuted"))
                        .child(row.source.label()),
                )
            })
            .tooltip({
                let command: SharedString = row.command.clone().into();
                move |window, cx| Tooltip::new(command.clone()).build(window, cx)
            });
        let when = h_flex()
            .gap_1p5()
            .child(div().text_color(tint("textMuted", 0.7)).child("When"))
            .child(div().font_family("Menlo").child(if row.when.is_empty() {
                "Always".to_owned()
            } else {
                row.when.clone()
            }));
        let pill = match recording {
            Some(recording) => capture_box(&recording.key).into_any_element(),
            None => {
                let target = row.clone();
                div()
                    .id(("keybinding-key", index))
                    .cursor_pointer()
                    .rounded(px(6.))
                    .hover(|pill| pill.bg(tint("accentSurface", 0.6)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.start_recording(Some(target.clone()), window, cx)
                    }))
                    .child(key_group(&row.key))
                    .into_any_element()
            }
        };
        let save = recording
            .filter(|recording| !recording.key.is_empty() && recording.key != row.key)
            .map(|_| {
                Button::new(("save-keybinding", index))
                    .outline()
                    .xsmall()
                    .label(if saving { "Saving" } else { "Save" })
                    .disabled(saving)
                    .on_click(cx.listener(|view, _, _, cx| view.save_recording(cx)))
            });
        let menu = (row.can_reset() || row.can_remove()).then(|| {
            let owner = cx.entity().downgrade();
            let target = row.clone();
            Button::new(("keybinding-menu", index))
                .icon(icon("ellipsis"))
                .ghost()
                .xsmall()
                .disabled(saving)
                .accessibility_label(format!("Actions for {}", row.title))
                .dropdown_menu(move |mut menu, _, _| {
                    if target.can_reset() {
                        let (owner, row) = (owner.clone(), target.clone());
                        menu = menu.item(PopupMenuItem::new("Reset to default").on_click(
                            move |_, _, cx| {
                                let _ = owner
                                    .update(cx, |view, cx| view.reset_keybinding(row.clone(), cx));
                            },
                        ));
                    }
                    if target.can_remove() {
                        let (owner, row) = (owner.clone(), target.clone());
                        menu = menu.item(PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                            let _ = owner
                                .update(cx, |view, cx| view.remove_keybinding(row.clone(), cx));
                        }));
                    }
                    menu
                })
        });
        div()
            .id(("keybinding-row", index))
            .group(group.clone())
            .child(
                Row::new(title)
                    .description(when)
                    .control(
                        h_flex()
                            .gap_1()
                            .children(conflict_warning(("keybinding-conflict", index), &conflicts))
                            .child(
                                div()
                                    .invisible()
                                    .group_hover(group, |menu| menu.visible())
                                    .children(menu),
                            )
                            .child(pill)
                            .children(save),
                    )
                    .render(),
            )
            .into_any_element()
    }

    fn new_binding_row(
        &self,
        recording: &Recording,
        all_rows: &[KeybindingRow],
        rules: &[agent_protocol::keybindings::KeybindingRule],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let commands: Vec<Choice> = keybinding_command_options(rules)
            .into_iter()
            .map(|command| Choice {
                label: command_label(&command),
                selected: recording.command.as_deref() == Some(command.as_str()),
                id: command,
                description: None,
                icon: None,
            })
            .collect();
        let conflicts = keybinding_conflict_labels(all_rows, "new", &recording.key, "");
        let saving = self.settings.keybindings.saving.is_some();
        let ready = recording.command.is_some() && !recording.key.is_empty() && !saving;
        div()
            .bg(tint("muted", 0.15))
            .child(
                Row::new("New keybinding")
                    .description(
                        h_flex()
                            .gap_1p5()
                            .child(div().text_color(tint("textMuted", 0.7)).child("When"))
                            .child(div().font_family("Menlo").child("Always")),
                    )
                    .control(
                        h_flex()
                            .gap_2()
                            .child(select_sized(
                                "new-keybinding-command",
                                recording
                                    .command
                                    .as_deref()
                                    .map_or_else(|| "Command".into(), command_label),
                                commands,
                                224.,
                                |view, command, window, cx| {
                                    if let Some(recording) =
                                        view.settings.keybindings.recording.as_mut()
                                    {
                                        recording.command = Some(command);
                                    }
                                    window.focus(&view.settings.keybindings.capture, cx);
                                    cx.notify();
                                },
                                cx,
                            ))
                            .children(conflict_warning("new-keybinding-conflict", &conflicts))
                            .child(capture_box(&recording.key))
                            .child(
                                Button::new("save-new-keybinding")
                                    .small()
                                    .label(if saving { "Saving" } else { "Save" })
                                    .disabled(!ready)
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.save_recording(cx)),
                                    ),
                            )
                            .child(
                                Button::new("cancel-new-keybinding")
                                    .icon(icon("x"))
                                    .ghost()
                                    .xsmall()
                                    .disabled(saving)
                                    .tooltip("Cancel")
                                    .accessibility_label("Cancel new keybinding")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.settings.keybindings.recording = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .render(),
            )
            .into_any_element()
    }
}

/// The field that shows the shortcut being pressed.
fn capture_box(key: &str) -> impl IntoElement {
    h_flex()
        .h_7()
        .w(px(176.))
        .px_2()
        .rounded(px(6.))
        .border_1()
        .border_color(color("focus"))
        .text_xs()
        .font_family("Menlo")
        .map(|field| {
            if key.is_empty() {
                field.text_color(color("textMuted")).child("Press shortcut")
            } else {
                field.child(key_group(key))
            }
        })
}
