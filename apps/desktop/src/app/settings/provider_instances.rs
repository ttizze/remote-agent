//! Native editor for the Host-owned provider instance map.
//!
//! The form owns transient input widgets only.  Provider ids, environment
//! parsing, model normalization, and complete-map validation live in
//! `agent_core::view::provider_instances`; saving sends one core intent so the
//! Host remains the owner of persisted launch configuration.

use super::{Row, notice, section};
use crate::app::{Desktop, ui::{color, icon, tint}};
use agent_core::{
    state::Intent,
    view::provider_instances as editing,
};
use agent_domain::Driver;
use agent_protocol::models::{ProviderCustomModel, ProviderInstanceConfig};
use gpui_kit::{
    component::{
        Disableable, Sizable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        notification::Notification,
        switch::Switch,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::BTreeMap;

fn driver_label(driver: Driver) -> &'static str {
    match driver {
        Driver::Codex => "Codex",
        Driver::Claude => "Claude",
    }
}

fn effective_instances(view: &Desktop) -> BTreeMap<String, ProviderInstanceConfig> {
    let mut instances = view
        .snapshot
        .host_settings
        .as_ref()
        .map(|settings| settings.provider_instances.clone())
        .unwrap_or_default();
    // A built-in provider can be live before the settings map contains an
    // override.  Present that live instance as an editable baseline; saving
    // it creates the explicit override the Host already supports.
    if let Some(providers) = &view.snapshot.providers {
        for provider in providers {
            instances.entry(provider.instance.clone()).or_insert_with(|| {
                ProviderInstanceConfig {
                    driver: provider.driver,
                    display_name: provider.display_name.clone(),
                    enabled: provider.enabled,
                    ..Default::default()
                }
            });
        }
    }
    instances
}

fn config_for_instance(view: &Desktop, instance_id: &str) -> Option<ProviderInstanceConfig> {
    effective_instances(view).remove(instance_id)
}

/// Opens the add or edit form.  The current map is read again on save so a
/// refresh or another settings write cannot be overwritten by an old dialog.
pub(super) fn open(
    view: &mut Desktop,
    instance_id: Option<String>,
    window: &mut Window,
    cx: &mut Context<Desktop>,
) {
    let initial = instance_id
        .as_deref()
        .and_then(|id| config_for_instance(view, id))
        .unwrap_or_default();
    let desktop = cx.entity().downgrade();
    let editing = instance_id.is_some();
    let removable = instance_id
        .as_deref()
        .is_some_and(|id| !matches!(id, "codex" | "claude"));
    let editor = cx.new(|cx| {
        ProviderInstanceEditor::new(desktop, instance_id, removable, initial, window, cx)
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(if editing {
                "Edit provider instance"
            } else {
                "Add provider instance"
            })
            .w(px(680.))
            .child(editor.clone())
    });
}

impl Desktop {
    /// Renders explicit Host overrides together with live built-in slots.
    pub(super) fn render_provider_instances(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let instances = effective_instances(self);
        let add = Button::new("provider-instance-add")
            .outline()
            .xsmall()
            .icon(icon("plus"))
            .label("Add instance")
            .on_click(cx.listener(|view, _, window, cx| open(view, None, window, cx)));
        let mut rows = Vec::new();
        for (index, (instance_id, instance)) in instances.into_iter().enumerate() {
            let live = self
                .snapshot
                .providers
                .as_ref()
                .and_then(|providers| providers.iter().find(|provider| provider.instance == instance_id));
            let title = if instance.display_name.trim().is_empty() {
                format!("{} ({instance_id})", driver_label(instance.driver))
            } else {
                instance.display_name.clone()
            };
            let mut detail = format!(
                "{} · {} custom model{}{}",
                driver_label(instance.driver),
                instance.custom_models.len(),
                if instance.custom_models.len() == 1 { "" } else { "s" },
                if instance.enabled { "" } else { " · disabled" },
            );
            let update_instance = live
                .and_then(|provider| provider.version_advisory.as_ref())
                .filter(|advisory| advisory.can_update)
                .map(|_| instance_id.clone());
            if let Some(advisory) = live.and_then(|provider| provider.version_advisory.as_ref()) {
                let status = match advisory.status {
                    agent_protocol::models::ProviderVersionAdvisoryStatus::Current => "up to date",
                    agent_protocol::models::ProviderVersionAdvisoryStatus::BehindLatest => {
                        "update available"
                    }
                    agent_protocol::models::ProviderVersionAdvisoryStatus::Unknown => "version unknown",
                };
                detail.push_str(&format!(" · {status}"));
                if let Some(latest) = &advisory.latest_version {
                    detail.push_str(&format!(" ({latest})"));
                }
            }
            let edit_id = instance_id.clone();
            let mut controls = h_flex().gap(px(6.)).child(
                Button::new(("provider-instance-edit", index))
                    .outline()
                    .xsmall()
                    .label("Edit")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        open(view, Some(edit_id.clone()), window, cx)
                    })),
            );
            if let Some(update_instance) = update_instance {
                controls = controls.child(
                    Button::new(("provider-instance-update", index))
                        .outline()
                        .xsmall()
                        .label("Update")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            let instance = update_instance.clone();
                            view.perform_then(
                                Intent::UpdateProvider {
                                    instance,
                                    target_version: None,
                                },
                                move |view, result, window, cx| match result {
                                    Ok(_) => {
                                        window.push_notification(
                                            Notification::success("Provider update completed")
                                                .title("Provider updated"),
                                            cx,
                                        );
                                        view.perform(Intent::LoadProviders);
                                    }
                                    Err(error) => window.push_notification(
                                        Notification::error(
                                            agent_core::presentation::error::error_message(error),
                                        )
                                        .title("Provider update failed"),
                                        cx,
                                    ),
                                },
                            );
                        })),
                );
            }
            rows.push(
                Row::new(title)
                    .description(detail)
                    .control(controls)
                    .render(),
            );
        }
        if rows.is_empty() {
            rows.push(notice("Provider instances are still loading.").into_any_element());
        }
        section(
            Some("Provider instances".into()),
            Some(
                div()
                    .text_xs()
                    .text_color(tint("textMuted", 0.8))
                    .child("Configure the executable, environment, launch arguments, and custom models for each Host provider.")
                    .into_any_element(),
            ),
            Some(add.into_any_element()),
            rows,
        )
        .into_any_element()
    }
}

struct ModelEditor {
    slug: Entity<InputState>,
    name: Entity<InputState>,
    aliases: Entity<InputState>,
    badge: Entity<InputState>,
    options: Entity<TextareaState>,
    is_default: bool,
    is_legacy: bool,
}

impl ModelEditor {
    fn new(
        model: ProviderCustomModel,
        window: &mut Window,
        cx: &mut Context<ProviderInstanceEditor>,
    ) -> Self {
        let slug = cx.new(|cx| InputState::new(window, cx).placeholder("model-slug"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Display name"));
        let aliases = cx.new(|cx| InputState::new(window, cx).placeholder("alias, another-alias"));
        let badge = cx.new(|cx| InputState::new(window, cx).placeholder("new"));
        let options = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("[]")
                .auto_grow(2, 6)
        });
        slug.update(cx, |input, cx| input.set_value(model.slug, window, cx));
        name.update(cx, |input, cx| input.set_value(model.name, window, cx));
        aliases.update(cx, |input, cx| {
            input.set_value(model.aliases.join(", "), window, cx)
        });
        badge.update(cx, |input, cx| {
            input.set_value(model.badge.unwrap_or_default(), window, cx)
        });
        options.update(cx, |input, cx| {
            input.set_value(
                serde_json::to_string_pretty(&model.option_descriptors).unwrap_or_else(|_| "[]".into()),
                window,
                cx,
            )
        });
        Self {
            slug,
            name,
            aliases,
            badge,
            options,
            is_default: model.is_default,
            is_legacy: model.is_legacy,
        }
    }

    fn value(&self, cx: &App) -> Result<ProviderCustomModel, String> {
        let aliases = self
            .aliases
            .read(cx)
            .value()
            .split([',', '\n'])
            .map(str::to_owned)
            .collect();
        let options = self.options.read(cx).value().trim().to_owned();
        let option_descriptors = if options.is_empty() {
            vec![]
        } else {
            serde_json::from_str(&options)
                .map_err(|error| format!("Composer options must be valid JSON: {error}"))?
        };
        Ok(ProviderCustomModel {
            slug: self.slug.read(cx).value().to_string(),
            name: self.name.read(cx).value().to_string(),
            aliases,
            badge: Some(self.badge.read(cx).value().to_string()),
            is_default: self.is_default,
            is_legacy: self.is_legacy,
            option_descriptors,
        })
    }
}

struct ProviderInstanceEditor {
    desktop: WeakEntity<Desktop>,
    instance_id: Option<String>,
    removable: bool,
    id: Entity<InputState>,
    display_name: Entity<InputState>,
    accent_color: Entity<InputState>,
    binary_path: Entity<InputState>,
    home_path: Entity<InputState>,
    environment: Entity<TextareaState>,
    launch_args: Entity<TextareaState>,
    driver: Driver,
    enabled: bool,
    models: Vec<ModelEditor>,
    error: Option<String>,
    saving: bool,
}

impl ProviderInstanceEditor {
    fn new(
        desktop: WeakEntity<Desktop>,
        instance_id: Option<String>,
        removable: bool,
        initial: ProviderInstanceConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let id = cx.new(|cx| InputState::new(window, cx).placeholder("build"));
        let display_name = cx.new(|cx| InputState::new(window, cx).placeholder("Build provider"));
        let accent_color = cx.new(|cx| InputState::new(window, cx).placeholder("#rrggbb"));
        let binary_path = cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/provider"));
        let home_path = cx.new(|cx| InputState::new(window, cx).placeholder("~/.provider"));
        let environment = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("PROVIDER_MODE=work")
                .auto_grow(3, 8)
        });
        let launch_args = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("--profile\nwork")
                .auto_grow(2, 6)
        });
        id.update(cx, |input, cx| {
            input.set_value(instance_id.clone().unwrap_or_default(), window, cx)
        });
        display_name.update(cx, |input, cx| input.set_value(initial.display_name, window, cx));
        accent_color.update(cx, |input, cx| {
            input.set_value(initial.accent_color.unwrap_or_default(), window, cx)
        });
        binary_path.update(cx, |input, cx| {
            input.set_value(initial.binary_path.unwrap_or_default(), window, cx)
        });
        home_path.update(cx, |input, cx| {
            input.set_value(initial.home_path.unwrap_or_default(), window, cx)
        });
        environment.update(cx, |input, cx| {
            input.set_value(editing::format_environment_lines(&initial.environment), window, cx)
        });
        launch_args.update(cx, |input, cx| {
            input.set_value(editing::format_launch_args(&initial.launch_args), window, cx)
        });
        let mut editor = Self {
            desktop,
            instance_id,
            removable,
            id,
            display_name,
            accent_color,
            binary_path,
            home_path,
            environment,
            launch_args,
            driver: initial.driver,
            enabled: initial.enabled,
            models: vec![],
            error: None,
            saving: false,
        };
        editor.models = initial
            .custom_models
            .into_iter()
            .map(|model| ModelEditor::new(model, window, cx))
            .collect();
        editor
    }

    fn input(&self, cx: &App) -> Result<(String, ProviderInstanceConfig), String> {
        let id = self.id.read(cx).value().trim().to_owned();
        let environment = editing::parse_environment_lines(self.environment.read(cx).value())?;
        let custom_models = self
            .models
            .iter()
            .map(|model| model.value(cx))
            .collect::<Result<Vec<_>, _>>()?;
        let config = ProviderInstanceConfig {
            driver: self.driver,
            display_name: self.display_name.read(cx).value().to_string(),
            accent_color: Some(self.accent_color.read(cx).value().to_string()),
            enabled: self.enabled,
            binary_path: Some(self.binary_path.read(cx).value().to_string()),
            home_path: Some(self.home_path.read(cx).value().to_string()),
            environment,
            launch_args: editing::parse_launch_args(self.launch_args.read(cx).value()),
            custom_models,
        };
        let config = editing::normalize_config(&id, config)?;
        Ok((id, config))
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let (id, config) = match self.input(cx) {
            Ok(input) => input,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(desktop) = self.desktop.upgrade() else {
            return;
        };
        let Some(current) = desktop
            .read(cx)
            .snapshot
            .host_settings
            .as_ref()
            .map(|settings| settings.provider_instances.clone())
        else {
            self.error = Some("Host settings are still loading.".into());
            cx.notify();
            return;
        };
        let replacing = self.instance_id.is_some();
        if replacing && self.instance_id.as_deref() != Some(id.as_str()) {
            self.error = Some("An existing instance id cannot be changed.".into());
            cx.notify();
            return;
        }
        if !replacing && effective_instances(desktop.read(cx)).contains_key(&id) {
            self.error = Some(format!("Provider instance {id} already exists."));
            cx.notify();
            return;
        }
        let next = match editing::upsert(&current, id, config) {
            Ok(next) => next,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.saving = true;
        self.error = None;
        cx.notify();
        let editor = cx.entity().downgrade();
        let _ = desktop.update(cx, |view, _| {
            view.perform_then(
                Intent::SetProviderInstances {
                    provider_instances_json: serde_json::to_string(&next)
                        .expect("provider instance settings must serialize"),
                },
                move |_, result, window, cx| match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => {
                        let _ = editor.update(cx, |editor, cx| {
                            editor.saving = false;
                            editor.error = Some(error.clone());
                            cx.notify();
                        });
                    }
                },
            );
        });
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(instance_id) = self.instance_id.clone() else {
            return;
        };
        let name = self.display_name.read(cx).value().trim().to_owned();
        let editor = cx.entity().downgrade();
        let _ = self.desktop.update(cx, |view, cx| {
            view.confirm(
                crate::app::dialogs::Confirm {
                    title: Some(format!("Remove provider instance \"{name}\"?")),
                    message: "Existing threads keep their provider selection; new threads cannot use this instance after removal.".into(),
                    action: "Remove instance".into(),
                    destructive: true,
                },
                window,
                cx,
                move |view, _, cx| {
                    let Some(current) = view
                        .snapshot
                        .host_settings
                        .as_ref()
                        .map(|settings| settings.provider_instances.clone())
                    else {
                        let _ = editor.update(cx, |editor, cx| {
                            editor.error = Some("Host settings are still loading.".into());
                            cx.notify();
                        });
                        return;
                    };
                    let next = match editing::remove(&current, &instance_id) {
                        Ok(next) => next,
                        Err(error) => {
                            let _ = editor.update(cx, |editor, cx| {
                                editor.error = Some(error);
                                cx.notify();
                            });
                            return;
                        }
                    };
                    let editor_for_result = editor.clone();
                    view.perform_then(
                        Intent::SetProviderInstances {
                            provider_instances_json: serde_json::to_string(&next)
                                .expect("provider instance settings must serialize"),
                        },
                        move |_, result, window, cx| {
                            if let Err(error) = result {
                                let _ = editor_for_result.update(cx, |editor, cx| {
                                    editor.error = Some(error.clone());
                                    editor.saving = false;
                                    cx.notify();
                                });
                            } else {
                                window.close_dialog(cx);
                            }
                        },
                    );
                },
            );
        });
    }

    fn field(label: &'static str, control: impl IntoElement) -> Div {
        v_flex()
            .gap(px(5.))
            .child(div().text_sm().font_medium().child(label))
            .child(control)
    }

    fn toggle(
        id: &'static str,
        label: &'static str,
        checked: bool,
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
            .child(label)
            .child(
                Switch::new(id)
                    .checked(checked)
                    .accessibility_label(label)
                    .on_click(cx.listener(move |editor, checked: &bool, _, cx| {
                        editor.enabled = *checked;
                        cx.notify();
                    })),
            )
    }
}

impl Render for ProviderInstanceEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_driver = self.driver;
        let owner = cx.entity().downgrade();
        let driver_picker = Button::new("provider-instance-driver")
            .outline()
            .small()
            .label(driver_label(current_driver))
            .dropdown_menu(move |mut menu, _, _| {
                for driver in [Driver::Codex, Driver::Claude] {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(driver_label(driver))
                            .checked(driver == current_driver)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |editor, cx| {
                                    editor.driver = driver;
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        let existing = self.instance_id.is_some();
        let model_count = self.models.len();
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("Provider settings are stored on the connected Host and apply to new provider sessions."),
            )
            .child(Self::field("Provider", driver_picker))
            .child(Self::field(
                "Instance ID",
                if existing {
                    div()
                        .px_3()
                        .py_2()
                        .rounded(px(6.))
                        .bg(tint("muted", 0.35))
                        .font_family("monospace")
                        .text_sm()
                        .child(self.id.read(cx).value().to_string())
                        .into_any_element()
                } else {
                    Input::new(&self.id).small().into_any_element()
                },
            ))
            .child(Self::field(
                "Display name",
                Input::new(&self.display_name).small().aria_label("Provider display name"),
            ))
            .child(Self::field(
                "Accent color",
                Input::new(&self.accent_color).small().aria_label("Provider accent color"),
            ))
            .child(Self::toggle("provider-instance-enabled", "Enabled", self.enabled, cx))
            .child(
                v_flex()
                    .gap_3()
                    .pt_2()
                    .child(div().font_medium().text_sm().child("Runtime"))
                    .child(Self::field(
                        "Executable path (optional)",
                        Input::new(&self.binary_path).small().aria_label("Provider executable path"),
                    ))
                    .child(Self::field(
                        "Home path (optional)",
                        Input::new(&self.home_path).small().aria_label("Provider home path"),
                    ))
                    .child(Self::field(
                        "Environment variables",
                        Textarea::new(&self.environment).aria_label("Provider environment variables"),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(color("textMuted"))
                            .child("One NAME=value entry per line. Values are kept on the Host."),
                    )
                    .child(Self::field(
                        "Launch arguments",
                        Textarea::new(&self.launch_args).aria_label("Provider launch arguments"),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(color("textMuted"))
                            .child("One argument per line; arguments are appended without shell expansion."),
                    ),
            )
            .child(
                v_flex()
                    .gap_3()
                    .pt_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().font_medium().text_sm().child("Custom models"))
                            .child(
                                Button::new("provider-instance-add-model")
                                    .outline()
                                    .xsmall()
                                    .icon(icon("plus"))
                                    .label("Add model")
                                    .on_click(cx.listener(|editor, _, window, cx| {
                                        editor.models.push(ModelEditor::new(
                                            ProviderCustomModel::default(),
                                            window,
                                            cx,
                                        ));
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(if model_count == 0 {
                        div()
                            .text_sm()
                            .text_color(color("textMuted"))
                            .child("No custom models configured.")
                            .into_any_element()
                    } else {
                        v_flex()
                            .gap_3()
                            .children(self.models.iter().enumerate().map(|(index, model)| {
                                let remove_id = ("provider-instance-remove-model", index);
                                h_flex()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .flex_1()
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(Input::new(&model.slug).small().flex_1().aria_label("Custom model slug"))
                                                    .child(Input::new(&model.name).small().flex_1().aria_label("Custom model name")),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(Input::new(&model.aliases).small().flex_1().aria_label("Custom model aliases"))
                                                    .child(Input::new(&model.badge).small().w(px(110.)).aria_label("Custom model badge")),
                                            )
                                            .child(Textarea::new(&model.options).aria_label("Custom model composer options JSON"))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(color("textMuted"))
                                                    .child("Composer options use the protocol OptionDescriptor JSON format."),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_3()
                                                    .child(
                                                        Switch::new(SharedString::from(format!(
                                                            "provider-model-default-{index}"
                                                        )))
                                                            .checked(model.is_default)
                                                            .accessibility_label("Custom model default")
                                                            .on_click(cx.listener(move |editor, checked: &bool, _, cx| {
                                                                if let Some(model) = editor.models.get_mut(index) {
                                                                    model.is_default = *checked;
                                                                }
                                                                cx.notify();
                                                            })),
                                                    )
                                                    .child(div().text_xs().text_color(color("textMuted")).child("Default"))
                                                    .child(
                                                        Switch::new(SharedString::from(format!(
                                                            "provider-model-legacy-{index}"
                                                        )))
                                                            .checked(model.is_legacy)
                                                            .accessibility_label("Custom model legacy")
                                                            .on_click(cx.listener(move |editor, checked: &bool, _, cx| {
                                                                if let Some(model) = editor.models.get_mut(index) {
                                                                    model.is_legacy = *checked;
                                                                }
                                                                cx.notify();
                                                            })),
                                                    )
                                                    .child(div().text_xs().text_color(color("textMuted")).child("Legacy")),
                                            ),
                                    )
                                    .child(
                                        Button::new(remove_id)
                                            .ghost()
                                            .small()
                                            .icon(icon("trash-2"))
                                            .accessibility_label("Remove custom model")
                                            .on_click(cx.listener(move |editor, _, _, cx| {
                                                editor.models.remove(index);
                                                cx.notify();
                                            })),
                                    )
                                    .into_any_element()
                            }))
                            .into_any_element()
                    }),
            )
            .when_some(self.error.clone(), |form, error| {
                form.child(div().text_sm().text_color(color("errorForeground")).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .when(self.removable, |footer| {
                        footer.child(
                            Button::new("provider-instance-delete")
                                .outline()
                                .label("Remove")
                                .text_color(color("errorForeground"))
                                .disabled(self.saving)
                                .on_click(cx.listener(|editor, _, window, cx| editor.delete(window, cx))),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("provider-instance-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("provider-instance-save")
                            .primary()
                            .label(if self.saving {
                                "Saving…"
                            } else if self.instance_id.is_some() {
                                "Save changes"
                            } else {
                                "Add instance"
                            })
                            .disabled(self.saving)
                            .on_click(cx.listener(|editor, _, _, cx| editor.save(cx))),
                    ),
            )
    }
}
