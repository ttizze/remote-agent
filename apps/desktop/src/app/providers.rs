use super::*;
use agent_core::presentation::provider_settings::{ProviderEditor, ProviderFieldKind};
use agent_protocol::providers::EnvironmentVariable;

pub(super) enum ProviderFormEvent {
    Save(Box<ProviderEditor>, bool),
    Close,
}

enum TextControl {
    Line(Entity<InputState>),
    Multiline(Entity<TextareaState>),
}
impl TextControl {
    fn new(
        value: &str,
        multiline: bool,
        window: &mut Window,
        cx: &mut Context<ProviderForm>,
    ) -> Self {
        if multiline {
            Self::Multiline(cx.new(|cx| {
                let mut input = TextareaState::new(window, cx).auto_grow(3, 10);
                input.set_value(value.to_owned(), window, cx);
                input
            }))
        } else {
            Self::Line(cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(value.to_owned(), window, cx);
                input
            }))
        }
    }
    fn value(&self, cx: &App) -> String {
        match self {
            Self::Line(input) => input.read(cx).value().to_string(),
            Self::Multiline(input) => input.read(cx).value().to_string(),
        }
    }
    fn element(&self, label: String, disabled: bool) -> AnyElement {
        match self {
            Self::Line(input) => Input::new(input)
                .aria_label(label)
                .disabled(disabled)
                .into_any_element(),
            Self::Multiline(input) => Textarea::new(input)
                .aria_label(label)
                .readonly(disabled)
                .into_any_element(),
        }
    }
}

struct EnvironmentInput {
    name: TextControl,
    value: Entity<InputState>,
    sensitive: bool,
    redacted: bool,
    _subscription: Subscription,
}

pub(super) struct ProviderForm {
    editor: ProviderEditor,
    id: TextControl,
    name: TextControl,
    accent: TextControl,
    fields: Vec<TextControl>,
    json: Option<TextControl>,
    environment: Vec<EnvironmentInput>,
    saving: bool,
    error: String,
    confirm_removal: bool,
}
impl EventEmitter<ProviderFormEvent> for ProviderForm {}

impl ProviderForm {
    pub(super) fn new(editor: ProviderEditor, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = editor
            .fields
            .iter()
            .map(|field| {
                TextControl::new(
                    &field.value,
                    field.kind == ProviderFieldKind::Json,
                    window,
                    cx,
                )
            })
            .collect();
        let environment = editor
            .environment
            .iter()
            .map(|variable| Self::environment_input(variable, window, cx))
            .collect();
        Self {
            id: TextControl::new(&editor.instance_id, false, window, cx),
            name: TextControl::new(&editor.display_name, false, window, cx),
            accent: TextControl::new(&editor.accent_color, false, window, cx),
            json: editor
                .fields
                .is_empty()
                .then(|| TextControl::new(&editor.config_json, true, window, cx)),
            editor,
            fields,
            environment,
            saving: false,
            error: String::new(),
            confirm_removal: false,
        }
    }
    fn environment_input(
        variable: &EnvironmentVariable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EnvironmentInput {
        let value = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .masked(variable.sensitive)
                .placeholder(if variable.value_redacted {
                    "保存済み（変更する場合だけ入力）"
                } else {
                    "値"
                });
            input.set_value(variable.value.clone(), window, cx);
            input
        });
        let subscription = cx.subscribe(&value, |form, input, event, _| {
            if matches!(event, InputEvent::Change)
                && let Some(variable) = form
                    .environment
                    .iter_mut()
                    .find(|variable| variable.value == input)
            {
                variable.redacted = false;
            }
        });
        EnvironmentInput {
            name: TextControl::new(&variable.name, false, window, cx),
            value,
            sensitive: variable.sensitive,
            redacted: variable.value_redacted,
            _subscription: subscription,
        }
    }
    fn save(&mut self, remove: bool, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let mut editor = self.editor.clone();
        editor.instance_id = self.id.value(cx);
        editor.display_name = self.name.value(cx);
        editor.accent_color = self.accent.value(cx);
        for (field, input) in editor.fields.iter_mut().zip(&self.fields) {
            field.value = input.value(cx);
        }
        if let Some(json) = &self.json {
            editor.config_json = json.value(cx);
        }
        editor.environment = self
            .environment
            .iter()
            .map(|variable| EnvironmentVariable {
                name: variable.name.value(cx),
                value: variable.value.read(cx).value().to_string(),
                sensitive: variable.sensitive,
                value_redacted: variable.redacted,
            })
            .collect();
        self.saving = true;
        self.error.clear();
        cx.emit(ProviderFormEvent::Save(Box::new(editor), remove));
        cx.notify();
    }
    pub(super) fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.saving = false;
        self.error = error;
        cx.notify();
    }
}

impl Render for ProviderForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.saving;
        let row = |label: String, element: AnyElement| {
            v_flex()
                .gap_2()
                .child(div().text_sm().child(label))
                .child(element)
        };
        let mut body = v_flex()
            .gap_5()
            .child(row(
                "ID".into(),
                self.id.element(
                    "Provider ID".into(),
                    disabled || self.editor.existing_id.is_some(),
                ),
            ))
            .child(
                div()
                    .text_sm()
                    .child(format!("Driver: {}", self.editor.driver)),
            )
            .child(row(
                "表示名".into(),
                self.name.element("Providerの表示名".into(), disabled),
            ))
            .child(row(
                "アクセントカラー".into(),
                self.accent.element("アクセントカラー".into(), disabled),
            ))
            .child(
                switch::Switch::new("provider-enabled")
                    .label("有効")
                    .checked(self.editor.enabled)
                    .disabled(disabled)
                    .on_click(cx.listener(|form, value, _, cx| {
                        form.editor.enabled = *value;
                        cx.notify();
                    })),
            );
        for (index, field) in self.editor.fields.iter().enumerate() {
            let element = self.fields[index].element(field.label.clone(), disabled);
            body = body.child(row(field.label.clone(), element));
        }
        if let Some(json) = &self.json {
            body = body.child(row(
                "Driver設定のJSON".into(),
                json.element("Driver設定のJSON".into(), disabled),
            ));
        }
        body = body.child(div().text_lg().font_semibold().child("環境変数"));
        for (index, variable) in self.environment.iter().enumerate() {
            body = body.child(
                v_flex()
                    .gap_2()
                    .pb_4()
                    .border_b_1()
                    .border_color(rgb(0x2b2f35))
                    .child(variable.name.element("環境変数の名前".into(), disabled))
                    .child(
                        Input::new(&variable.value)
                            .aria_label("環境変数の値")
                            .disabled(disabled),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                switch::Switch::new(format!("provider-sensitive-{index}"))
                                    .label("秘密値")
                                    .checked(variable.sensitive)
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |form, value, window, cx| {
                                        form.environment[index].sensitive = *value;
                                        form.environment[index].value.update(cx, |input, cx| {
                                            input.toggle_masked(window, cx)
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(format!("provider-remove-variable-{index}"))
                                    .label("削除")
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |form, _, _, cx| {
                                        form.environment.remove(index);
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        body = body.child(
            Button::new("provider-add-variable")
                .label("環境変数を追加")
                .disabled(disabled)
                .on_click(cx.listener(|form, _, window, cx| {
                    form.environment.push(Self::environment_input(
                        &EnvironmentVariable {
                            name: String::new(),
                            value: String::new(),
                            sensitive: false,
                            value_redacted: false,
                        },
                        window,
                        cx,
                    ));
                    cx.notify();
                })),
        );
        if !self.error.is_empty() {
            body = body.child(div().text_color(rgb(0xff8e86)).child(self.error.clone()));
        }
        body = body.child(
            h_flex()
                .gap_3()
                .child(
                    Button::new("provider-save")
                        .label(if self.saving {
                            "保存中…"
                        } else {
                            "保存"
                        })
                        .disabled(disabled)
                        .on_click(cx.listener(|form, _, _, cx| form.save(false, cx))),
                )
                .child(
                    Button::new("provider-cancel")
                        .label("キャンセル")
                        .disabled(disabled)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ProviderFormEvent::Close))),
                ),
        );
        if self.editor.existing_id.is_some() {
            body = body.child(
                Button::new("provider-remove")
                    .label(if self.confirm_removal {
                        "削除を確定"
                    } else {
                        "Providerを削除"
                    })
                    .disabled(disabled)
                    .on_click(cx.listener(|form, _, _, cx| {
                        if form.confirm_removal {
                            form.save(true, cx);
                        } else {
                            form.confirm_removal = true;
                            cx.notify();
                        }
                    })),
            );
        }
        body
    }
}

impl Desktop {
    pub(super) fn edit_provider(
        &mut self,
        id: Option<String>,
        driver: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.snapshot.provider_editor(id, driver) else {
            return;
        };
        let form = cx.new(|cx| ProviderForm::new(editor, window, cx));
        self.provider_form_subscription = Some(cx.subscribe(&form, |view, _, event, cx| {
            match event {
                ProviderFormEvent::Save(editor, remove) => view.perform(
                    Intent::ApplyProviderEdit(op::ApplyProviderEdit {
                        editor: editor.as_ref().clone(),
                        remove: *remove,
                    }),
                    OperationCompletion::ProviderSettings,
                ),
                ProviderFormEvent::Close => {
                    view.provider_form = None;
                    view.provider_form_subscription = None;
                }
            }
            cx.notify();
        }));
        self.provider_form = Some(form);
        cx.notify();
    }
}
