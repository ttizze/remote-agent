//! The Keybindings page: every command's shortcuts, searchable, with
//! recording a new shortcut, adding a binding and resetting to defaults.
use super::{Choice, Row, page_container, section, select_sized};
use crate::app::{
    Desktop,
    keymap::{Binding, Keymap, Source, command_label, is_modifier_only, key_caps, keystroke_key},
    ui::{color, icon, text_2xs, tint},
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
    target: Option<Binding>,
    command: Option<String>,
    key: String,
}

pub(super) struct KeybindingsState {
    search: Entity<InputState>,
    search_open: bool,
    recording: Option<Recording>,
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
        .children(key_caps(key).into_iter().map(|cap| {
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

/// Whether a binding matches the search.
fn matches_search(binding: &Binding, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let source = match binding.source {
        Source::Default => "default",
        Source::Custom => "custom",
    };
    [
        binding.command.as_str(),
        &command_label(&binding.command),
        &binding.key,
        binding.when.as_deref().unwrap_or_default(),
        source,
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(&query))
}

impl Desktop {
    fn start_recording(
        &mut self,
        target: Option<Binding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = target.as_ref().map(|binding| binding.command.clone());
        self.settings.keybindings.recording = Some(Recording {
            target,
            command,
            key: String::new(),
        });
        window.focus(&self.settings.keybindings.capture, cx);
        cx.notify();
    }

    fn save_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = self.settings.keybindings.recording.take() else {
            return;
        };
        let Some(command) = recording.command.clone() else {
            self.settings.keybindings.recording = Some(recording);
            return;
        };
        let binding = Binding {
            key: recording.key.clone(),
            command,
            when: recording
                .target
                .as_ref()
                .and_then(|target| target.when.clone()),
            source: Source::Custom,
        };
        if let Err(error) = self.keymap.upsert(binding, recording.target.as_ref()) {
            window.push_notification(
                Notification::error(error).title("Unable to save keybinding"),
                cx,
            );
            self.settings.keybindings.recording = Some(recording);
        }
        cx.notify();
    }

    pub(super) fn render_keybindings(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.settings.keybindings;
        let query = state.search.read(cx).value().to_string();
        let bindings = self.keymap.bindings();
        let count = bindings.len();
        let shown: Vec<Binding> = bindings
            .into_iter()
            .filter(|binding| matches_search(binding, &query))
            .collect();
        let mut rows: Vec<AnyElement> = vec![];
        if let Some(recording) = state
            .recording
            .as_ref()
            .filter(|recording| recording.target.is_none())
        {
            rows.push(self.new_binding_row(recording, cx));
        }
        if shown.is_empty() {
            rows.push(
                div()
                    .px_4()
                    .py_3()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("No keybindings match your search.")
                    .into_any_element(),
            );
        }
        for (index, binding) in shown.into_iter().enumerate() {
            rows.push(self.binding_row(index, binding, cx));
        }
        let search = if state.search_open {
            Input::new(&state.search)
                .small()
                .w(px(176.))
                .prefix(icon("search").size(px(14.)).text_color(color("textMuted")))
                .into_any_element()
        } else {
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
                }))
                .into_any_element()
        };
        let actions = h_flex()
            .gap(px(6.))
            .child(
                text_2xs(div())
                    .text_color(color("textMuted"))
                    .child(format!(
                        "{count} {}",
                        if count == 1 { "binding" } else { "bindings" }
                    )),
            )
            .child(search)
            .child(
                Button::new("add-keybinding")
                    .outline()
                    .xsmall()
                    .icon(icon("plus"))
                    .label("Add keybinding")
                    .on_click(
                        cx.listener(|view, _, window, cx| view.start_recording(None, window, cx)),
                    ),
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
                if keystroke.key == "f" && keystroke.modifiers.secondary() {
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
                } else if !is_modifier_only(keystroke) {
                    recording.key = keystroke_key(keystroke);
                }
                cx.notify();
            }))
            .child(page)
            .into_any_element()
    }

    fn binding_row(&self, index: usize, binding: Binding, cx: &mut Context<Self>) -> AnyElement {
        let recording = self
            .settings
            .keybindings
            .recording
            .as_ref()
            .filter(|recording| recording.target.as_ref() == Some(&binding));
        let conflicts = self.keymap.conflicts(&binding.key, &binding.command);
        let group = SharedString::from(format!("keybinding-{index}"));
        let custom = binding.source == Source::Custom;
        let title = h_flex()
            .id(("keybinding-title", index))
            .gap_2()
            .child(command_label(&binding.command))
            .when(custom, |title| {
                title.child(
                    text_2xs(div())
                        .px_1p5()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(tint("border", 0.6))
                        .text_color(color("textMuted"))
                        .child("Custom"),
                )
            })
            .tooltip({
                let command: SharedString = binding.command.clone().into();
                move |window, cx| Tooltip::new(command.clone()).build(window, cx)
            });
        let when = h_flex()
            .gap_1p5()
            .child(div().text_color(tint("textMuted", 0.7)).child("When"))
            .child(
                div()
                    .font_family("Menlo")
                    .child(binding.when.clone().unwrap_or_else(|| "Always".into())),
            );
        let pill = match recording {
            Some(recording) => capture_box(&recording.key).into_any_element(),
            None => {
                let target = binding.clone();
                div()
                    .id(("keybinding-key", index))
                    .cursor_pointer()
                    .rounded(px(6.))
                    .hover(|pill| pill.bg(tint("accentSurface", 0.6)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.start_recording(Some(target.clone()), window, cx)
                    }))
                    .child(key_group(&binding.key))
                    .into_any_element()
            }
        };
        let save = recording
            .filter(|recording| !recording.key.is_empty() && recording.key != binding.key)
            .map(|_| {
                Button::new(("save-keybinding", index))
                    .outline()
                    .xsmall()
                    .label("Save")
                    .on_click(cx.listener(|view, _, window, cx| view.save_recording(window, cx)))
            });
        let menu = {
            let owner = cx.entity().downgrade();
            let target = binding.clone();
            let resettable = custom && Keymap::has_default(&binding.command);
            div()
                .invisible()
                .group_hover(group.clone(), |menu| menu.visible())
                .when(custom, |menu| {
                    menu.child(
                        Button::new(("keybinding-menu", index))
                            .icon(icon("ellipsis"))
                            .ghost()
                            .xsmall()
                            .accessibility_label("Keybinding actions")
                            .dropdown_menu(move |mut menu, _, _| {
                                if resettable {
                                    let (owner, command) = (owner.clone(), target.command.clone());
                                    menu =
                                        menu.item(PopupMenuItem::new("Reset to default").on_click(
                                            move |_, _, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.keymap.reset(&command);
                                                    cx.notify();
                                                });
                                            },
                                        ));
                                }
                                let (owner, target) = (owner.clone(), target.clone());
                                menu.item(PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |view, cx| {
                                        view.keymap.remove(&target);
                                        cx.notify();
                                    });
                                }))
                            }),
                    )
                })
        };
        let conflict = (!conflicts.is_empty()).then(|| {
            let text: SharedString = format!(
                "Also bound to {}",
                conflicts
                    .iter()
                    .map(|command| command_label(command))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into();
            div()
                .id(("keybinding-conflict", index))
                .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
                .child(
                    icon("triangle-alert")
                        .size(px(14.))
                        .text_color(color("warning")),
                )
        });
        div()
            .id(("keybinding-row", index))
            .group(group)
            .child(
                Row::new(title)
                    .description(when)
                    .control(
                        h_flex()
                            .gap_1()
                            .children(conflict)
                            .child(menu)
                            .child(pill)
                            .children(save),
                    )
                    .render(),
            )
            .into_any_element()
    }

    fn new_binding_row(&self, recording: &Recording, cx: &mut Context<Self>) -> AnyElement {
        let commands: Vec<Choice> = Keymap::commands()
            .into_iter()
            .map(|command| Choice {
                id: command.into(),
                label: command_label(command),
                description: None,
                icon: None,
                selected: recording.command.as_deref() == Some(command),
            })
            .collect();
        let conflicts = recording
            .command
            .as_ref()
            .map(|command| self.keymap.conflicts(&recording.key, command))
            .unwrap_or_default();
        let ready = recording.command.is_some() && !recording.key.is_empty();
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
                            .gap_1()
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
                            .when(!conflicts.is_empty(), |row| {
                                row.child(
                                    icon("triangle-alert")
                                        .size(px(14.))
                                        .text_color(color("warning")),
                                )
                            })
                            .child(capture_box(&recording.key))
                            .child(
                                Button::new("save-new-keybinding")
                                    .outline()
                                    .xsmall()
                                    .label("Save")
                                    .disabled(!ready)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.save_recording(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("cancel-new-keybinding")
                                    .icon(icon("x"))
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Cancel")
                                    .accessibility_label("Cancel")
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

#[cfg(test)]
mod tests {
    use super::matches_search;
    use crate::app::keymap::{Binding, Source};
    use core::prelude::v1::test;

    #[test]
    fn search_matches_the_id_title_key_and_context() {
        let binding = Binding {
            key: "mod+shift+d".into(),
            command: "terminal.splitVertical".into(),
            when: Some("terminalFocus".into()),
            source: Source::Default,
        };
        for query in [
            "",
            "split vertical",
            "terminal.split",
            "shift+d",
            "terminalfocus",
        ] {
            assert!(matches_search(&binding, query), "{query}");
        }
        assert!(!matches_search(&binding, "sidebar"));
    }
}
