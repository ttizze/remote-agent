//! The add/edit dialog of a project action.
use super::projects::script_icon;
use crate::app::{
    Desktop,
    ui::{color, icon, tint},
};
use agent_core::{
    state::Intent,
    view::projects::scripts::{
        ProjectScriptInput, add_project_script, delete_project_script, project_script_input,
        update_project_script, validate_project_script_input,
    },
};
use agent_protocol::models::{ProjectScript, ProjectScriptIcon};
use gpui_kit::{
    component::{
        Disableable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        switch::Switch,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

const ICONS: [(ProjectScriptIcon, &str); 6] = [
    (ProjectScriptIcon::Play, "Play"),
    (ProjectScriptIcon::Test, "Test"),
    (ProjectScriptIcon::Lint, "Lint"),
    (ProjectScriptIcon::Configure, "Configure"),
    (ProjectScriptIcon::Build, "Build"),
    (ProjectScriptIcon::Debug, "Debug"),
];

/// Opens the dialog for a new action, or for `script_id`.
pub(super) fn open(
    view: &mut Desktop,
    project_id: String,
    script_id: Option<String>,
    window: &mut Window,
    cx: &mut Context<Desktop>,
) {
    let initial = script_id
        .as_ref()
        .and_then(|id| {
            project_scripts(view, &project_id)
                .into_iter()
                .find(|s| &s.id == id)
        })
        .map(|script| project_script_input(&script))
        .unwrap_or_default();
    let desktop = cx.entity().downgrade();
    let editing = script_id.is_some();
    let editor =
        cx.new(|cx| ScriptEditor::new(desktop, project_id, script_id, initial, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(if editing { "Edit Action" } else { "Add Action" })
            .w(px(512.))
            .child(editor.clone())
    });
}

fn project_scripts(view: &Desktop, project_id: &str) -> Vec<ProjectScript> {
    view.snapshot
        .shell_projects()
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| project.scripts.clone())
        .unwrap_or_default()
}

struct ScriptEditor {
    desktop: WeakEntity<Desktop>,
    project_id: String,
    script_id: Option<String>,
    icon: ProjectScriptIcon,
    run_on_worktree_create: bool,
    wait_for_setup: bool,
    auto_open_preview: bool,
    name: Entity<InputState>,
    command: Entity<TextareaState>,
    preview_url: Entity<InputState>,
    error: Option<String>,
    saving: bool,
}

impl ScriptEditor {
    fn new(
        desktop: WeakEntity<Desktop>,
        project_id: String,
        script_id: Option<String>,
        initial: ProjectScriptInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Test"));
        let command = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("bun test")
                .auto_grow(2, 6)
        });
        let preview_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:5173"));
        name.update(cx, |input, cx| {
            input.set_value(initial.name.clone(), window, cx)
        });
        command.update(cx, |input, cx| {
            input.set_value(initial.command.clone(), window, cx)
        });
        preview_url.update(cx, |input, cx| {
            input.set_value(initial.preview_url.clone().unwrap_or_default(), window, cx)
        });
        name.update(cx, |input, cx| input.focus(window, cx));
        Self {
            desktop,
            project_id,
            script_id,
            icon: initial.icon,
            run_on_worktree_create: initial.run_on_worktree_create,
            wait_for_setup: initial.wait_for_setup,
            auto_open_preview: initial.auto_open_preview,
            name,
            command,
            preview_url,
            error: None,
            saving: false,
        }
    }

    fn input(&self, cx: &App) -> ProjectScriptInput {
        let preview_url = self.preview_url.read(cx).value().to_string();
        ProjectScriptInput {
            name: self.name.read(cx).value().to_string(),
            command: self.command.read(cx).value().to_string(),
            icon: self.icon,
            run_on_worktree_create: self.run_on_worktree_create,
            wait_for_setup: self.wait_for_setup,
            preview_url: (!preview_url.trim().is_empty()).then_some(preview_url),
            auto_open_preview: self.auto_open_preview,
        }
    }

    /// Sends the project's new list, closing the dialog once it lands.
    fn save(&mut self, scripts: Vec<ProjectScript>, cx: &mut Context<Self>) {
        let editor = cx.entity().downgrade();
        let project_id = self.project_id.clone();
        self.saving = true;
        cx.notify();
        let _ = self.desktop.update(cx, |view, _| {
            save_scripts(view, project_id, scripts, editor)
        });
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let input = match validate_project_script_input(&self.input(cx)) {
            Ok(input) => input,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(current) = self
            .desktop
            .upgrade()
            .map(|desktop| project_scripts(desktop.read(cx), &self.project_id))
        else {
            return;
        };
        let scripts = match &self.script_id {
            None => add_project_script(&current, &input),
            Some(id) => match update_project_script(&current, id, &input) {
                Ok(scripts) => scripts,
                Err(error) => {
                    self.error = Some(error);
                    cx.notify();
                    return;
                }
            },
        };
        self.error = None;
        self.save(scripts, cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(script_id) = self.script_id.clone() else {
            return;
        };
        let name = self.name.read(cx).value().to_string();
        let editor = cx.entity().downgrade();
        let project_id = self.project_id.clone();
        let _ = self.desktop.update(cx, |view, cx| {
            view.confirm(
                crate::app::dialogs::Confirm {
                    title: Some(format!("Delete action \"{name}\"?")),
                    message: "This action cannot be undone.".into(),
                    action: "Delete action".into(),
                    destructive: true,
                },
                window,
                cx,
                move |view, _, cx| {
                    let scripts =
                        delete_project_script(&project_scripts(view, &project_id), &script_id);
                    let _ = editor.update(cx, |editor, cx| {
                        editor.saving = true;
                        cx.notify();
                    });
                    save_scripts(view, project_id.clone(), scripts, editor.clone());
                },
            );
        });
    }

    fn toggle(
        id: &'static str,
        label: &'static str,
        checked: bool,
        disabled: bool,
        set: fn(&mut Self, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .justify_between()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(8.))
            .border_1()
            .border_color(tint("border", 0.7))
            .text_sm()
            .when(disabled, |row| row.opacity(0.6))
            .child(label)
            .child(
                Switch::new(id)
                    .checked(checked)
                    .disabled(disabled)
                    .accessibility_label(label)
                    .on_click(cx.listener(move |editor, checked: &bool, _, cx| {
                        set(editor, *checked);
                        cx.notify();
                    })),
            )
    }
}

fn save_scripts(
    view: &mut Desktop,
    project_id: String,
    scripts: Vec<ProjectScript>,
    editor: WeakEntity<ScriptEditor>,
) {
    view.perform_then(
        Intent::UpdateProjectScripts {
            project_id,
            scripts,
        },
        move |_, result, window, cx| match result {
            Ok(_) => window.close_dialog(cx),
            Err(error) => {
                let error = agent_core::presentation::error::error_message(error);
                let _ = editor.update(cx, |editor, cx| {
                    editor.saving = false;
                    editor.error = Some(error);
                    cx.notify();
                });
            }
        },
    );
}

fn field(label: &'static str, control: impl IntoElement) -> Div {
    v_flex()
        .gap(px(6.))
        .child(div().text_sm().font_medium().child(label))
        .child(control)
}

impl Render for ScriptEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_preview = !self.preview_url.read(cx).value().trim().is_empty();
        let current = self.icon;
        let owner = cx.entity().downgrade();
        let icon_picker = Button::new("script-icon")
            .outline()
            .size(px(36.))
            .flex_shrink_0()
            .icon(icon(script_icon(self.icon)))
            .accessibility_label("Choose icon")
            .dropdown_menu(move |mut menu, _, _| {
                for (choice, label) in ICONS {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(icon(script_icon(choice)))
                            .checked(choice == current)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |editor, cx| {
                                    editor.icon = choice;
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(color("textMuted")).child(
                "Actions are project-scoped commands you can run from the top bar or keybindings.",
            ))
            .child(field(
                "Name",
                h_flex()
                    .gap_2()
                    .child(icon_picker)
                    .child(Input::new(&self.name).flex_1().aria_label("Name")),
            ))
            .child(field(
                "Command",
                Textarea::new(&self.command).aria_label("Command"),
            ))
            .child(
                field(
                    "Preview URL (optional)",
                    Input::new(&self.preview_url).aria_label("Preview URL"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child("Open this URL in the in-app preview when this action runs."),
                ),
            )
            .child(Self::toggle(
                "script-run-on-create",
                "Run automatically on worktree creation",
                self.run_on_worktree_create,
                false,
                |editor, checked| editor.run_on_worktree_create = checked,
                cx,
            ))
            .child(Self::toggle(
                "script-wait",
                "Wait for it to finish before the agent starts",
                self.wait_for_setup,
                !self.run_on_worktree_create,
                |editor, checked| editor.wait_for_setup = checked,
                cx,
            ))
            .child(Self::toggle(
                "script-auto-preview",
                "Open preview automatically when this action runs",
                self.auto_open_preview,
                !has_preview,
                |editor, checked| editor.auto_open_preview = checked,
                cx,
            ))
            .when_some(self.error.clone(), |form, error| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(color("errorForeground"))
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .when(self.script_id.is_some(), |footer| {
                        footer.child(
                            Button::new("script-delete")
                                .outline()
                                .label("Delete")
                                .text_color(color("errorForeground"))
                                .disabled(self.saving)
                                .on_click(
                                    cx.listener(|editor, _, window, cx| editor.delete(window, cx)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("script-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("script-save")
                            .primary()
                            .label(if self.saving {
                                "Saving…"
                            } else if self.script_id.is_some() {
                                "Save changes"
                            } else {
                                "Save action"
                            })
                            .disabled(self.saving)
                            .on_click(cx.listener(|editor, _, _, cx| editor.submit(cx))),
                    ),
            )
    }
}
