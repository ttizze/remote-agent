//! The list owns its project editor; Core owns project decisions and persistence.
use crate::application::{Application, ListScope, ListSource};
use crate::l10n::{localized, tr};
use crate::{native, shared::Shared};
use agent_core::{
    presentation::{
        list::{ProjectSummary, ThreadList},
        project_menu::{ProjectAction, ProjectMenuItem},
    },
    state::{Intent, operations as op},
};
use nsh_core::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(serde::Serialize, serde::Deserialize)]
struct Command {
    #[serde(flatten)]
    scope: ListScope,
    action: ProjectAction,
}

pub(crate) fn menu(
    project: &ProjectSummary,
    sections: &[String],
    scope: &ListScope,
    local: bool,
) -> Value {
    #[cfg(windows)]
    let reveal = tr("エクスプローラーで開く");
    #[cfg(not(windows))]
    let reveal = tr("フォルダを開く");
    json!(encode(
        agent_core::presentation::project_menu::project_menu(project, sections, local, reveal),
        scope,
    ))
}

fn encode(items: Vec<ProjectMenuItem>, scope: &ListScope) -> Vec<Value> {
    items.into_iter().map(|item| match item {
            ProjectMenuItem::Separator => json!({"separator": true}),
            ProjectMenuItem::Submenu { title, items } => json!({"title": localized(title), "children": encode(items, scope)}),
            ProjectMenuItem::Action { title, action, checked, enabled } => json!({
                "title": localized(title), "selected": checked, "enabled": enabled,
                "id": serde_json::to_string(&Command { scope: scope.clone(), action }).expect("project menu commands serialize"),
            }),
        }).collect()
}

pub(crate) fn header_menu(
    hidden: &[ProjectSummary],
    desktop: bool,
    selected_host: &str,
    sources: &[ListSource],
) -> Value {
    let mut items = Vec::new();
    if desktop {
        items.push(json!({"id": "add", "title": tr("フォルダをプロジェクトに追加…"), "enabled": sources.iter().any(|source| source.scope.host == selected_host && source.scope.host == "local" && source.connected)}));
    }
    if !hidden.is_empty() {
        let hidden: Vec<_> = sources
            .iter()
            .flat_map(|source| {
                encode(
                    hidden
                        .iter()
                        .filter(|project| project.host_id == source.scope.host)
                        .map(|project| ProjectMenuItem::Action {
                            title: project.name.clone(),
                            checked: false,
                            enabled: true,
                            action: ProjectAction::Dispatch(Box::new(Intent::SetProjectHidden {
                                project_id: project.id.clone(),
                                hidden: false,
                            })),
                        })
                        .collect(),
                    &source.scope,
                )
            })
            .collect();
        items.push(json!({"title": tr("非表示のプロジェクトを戻す"), "children": hidden}));
    }
    json!(items)
}

/// Adds a folder the user picks as a project of this device's Host, which
/// starts a chat in it; a WSL distro's folder goes to that distro's Host.
pub(crate) fn add_folder(
    application: Stored<Arc<Application>>,
    host: &str,
    sources: &[ListSource],
) {
    if !is_desktop() || host != "local" {
        return;
    }
    let Some(scope) = sources
        .iter()
        .find(|source| source.scope.host == host)
        .map(|source| source.scope.clone())
    else {
        return;
    };
    let choice = pick_paths(nsh_core::file_dialog::OpenDialog::folders());
    spawn(async move {
        match choice.await {
            Ok(Some(paths)) => {
                if let Some(path) = paths.into_iter().next() {
                    application.with(|application| {
                        match application.open_wsl_folder(&path, |cwd| {
                            Intent::AddProject(op::AddProject { cwd })
                        }) {
                            Ok(true) => {}
                            Ok(false) => {
                                application.list_action(
                                    &scope,
                                    Intent::AddProject(op::AddProject {
                                        cwd: path.to_string_lossy().into_owned(),
                                    }),
                                    false,
                                );
                            }
                            Err(reason) => native::message(tr("プロジェクトを追加"), reason),
                        }
                    });
                }
            }
            Ok(_) => {}
            Err(reason) => native::message(tr("プロジェクトを追加"), reason),
        }
    });
}

#[derive(Clone, PartialEq)]
struct Editor {
    id: String,
    scope: ListScope,
    project: String,
    name: String,
    section: String,
    error: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct Projects {
    editor: Signal<Option<Editor>>,
    application: Stored<Arc<Application>>,
    host: Memo<String>,
    sources: Memo<Vec<ListSource>>,
    list: Memo<Option<Shared<ThreadList>>>,
}

impl Projects {
    pub fn new(
        application: Stored<Arc<Application>>,
        host: Memo<String>,
        sources: Memo<Vec<ListSource>>,
        list: Memo<Option<Shared<ThreadList>>>,
    ) -> Self {
        Self {
            editor: signal(None),
            application,
            host,
            sources,
            list,
        }
    }

    fn same_scope(self, scope: &ListScope) -> bool {
        self.application
            .with(|application| application.snapshot().list_scope_matches(scope))
    }

    pub fn select(self, command: &str) {
        if command == "add" {
            self.sources.with_untracked(|sources| {
                add_folder(self.application, &self.host.get_untracked(), sources)
            });
            return;
        }
        let Ok(command) = serde_json::from_str::<Command>(command) else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let action = match command.action.into_intent(now) {
            Ok(intent) => {
                self.application
                    .with(|application| application.list_action(&command.scope, intent, false));
                return;
            }
            Err(action) => action,
        };
        if !self.same_scope(&command.scope) {
            return;
        }
        match action {
            ProjectAction::Edit(id) => {
                let project = self.list.with_untracked(|list| {
                    list.as_ref()?
                        .projects
                        .iter()
                        .find(|project| project.id == id && project.host_id == command.scope.host)
                        .map(|project| (project.name.clone(), project.section.clone()))
                });
                if let Some((name, section)) = project {
                    self.editor.set(Some(Editor {
                        id: uuid::Uuid::new_v4().to_string(),
                        scope: command.scope,
                        project: id,
                        name,
                        section: section.unwrap_or_default(),
                        error: None,
                    }));
                }
            }
            ProjectAction::OpenFolder(path) if command.scope.host == "local" => {
                #[cfg(target_os = "macos")]
                native::macos::open_directory(&path);
                // File Explorer opens a folder as its default app.
                #[cfg(windows)]
                spawn(async move {
                    if let Err(reason) = open_path(path).await {
                        native::message(tr("フォルダを開く"), reason);
                    }
                });
                #[cfg(not(desktop))]
                let _ = path;
            }
            ProjectAction::OpenFolder(_) => native::message(
                tr("フォルダを開く"),
                tr("接続先のフォルダはこの端末では開けません"),
            ),
            _ => unreachable!("project operations resolve in Core"),
        }
    }

    /// Saves the open editor's labels once Core accepts them; a rejection stays
    /// in the editor beside its fields.
    fn save(self, editor: Editor, name: &str, section: &str) {
        if !self.same_scope(&editor.scope) {
            self.editor.set(None);
            native::message(
                tr("プロジェクトを編集"),
                tr("接続先が変わりました。プロジェクトを開き直してください"),
            );
            return;
        }
        if let Err(reason) = agent_core::state::projects::validate_project_labels(name, section) {
            self.editor.update(|editor| {
                if let Some(editor) = editor {
                    editor.error = Some(localized(reason.into()));
                }
            });
            return;
        }
        self.application.with(|application| {
            application.list_action(
                &editor.scope,
                Intent::EditProject {
                    project_id: editor.project,
                    name: name.into(),
                    section: section.into(),
                },
                false,
            )
        });
        self.editor.set(None);
    }

    /// The project editor: an NSH dialog on the desktop, the platform's sheet on
    /// phones.
    pub fn view(self) -> AnyElement {
        if is_desktop() {
            return when(
                move || self.editor.with(Option::is_some),
                move || self.dialog(),
            )
            .into_element();
        }
        native("bex.details")
            .prop("data", move || {
                self.editor.with(|editor| editor_data(editor.as_ref()))
            })
            .on_native_event(move |event: &NativeEvent| {
                let Some(editor) = self
                    .editor
                    .get_untracked()
                    .filter(|editor| event.data["token"].as_str() == Some(editor.id.as_str()))
                else {
                    return;
                };
                match (
                    event.data["action"].as_str(),
                    event.data["name"].as_str(),
                    event.data["section"].as_str(),
                ) {
                    (Some("cancel"), ..) => self.editor.set(None),
                    (Some("save"), Some(name), Some(section)) => self.save(editor, name, section),
                    _ => {}
                }
            })
            .w(px(0.0))
            .h(px(0.0))
            .into_element()
    }

    /// T3 Code's dialog over the window: the title and what editing keeps, the
    /// display name and section, then cancelling or saving.
    fn dialog(self) -> impl IntoElement {
        use crate::ui::{self, colors};
        use agent_core::presentation::conversation::ActionTone;
        use nsh_core::element::{overlay, rgba};
        let Some(editor) = self.editor.get_untracked() else {
            return view().into_element();
        };
        let name = signal(editor.name.clone());
        let section = signal(editor.section.clone());
        let save = move || {
            if let Some(editor) = self.editor.get_untracked() {
                self.save(editor, &name.get_untracked(), &section.get_untracked());
            }
        };
        let cancel = move || self.editor.set(None);
        let error = move || self.editor.with(|editor| editor.as_ref()?.error.clone());
        let label = |title: &'static str| {
            text(title)
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
        };
        overlay()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .right(px(0.0))
            .bottom(px(0.0))
            .items_center()
            .justify_center()
            .bg(rgba(0x00000052))
            .backdrop_blur(px(4.0))
            .on_press(cancel)
            .child(
                view()
                    .w(px(448.0))
                    .rounded(px(16.0))
                    .border_1()
                    .border_color(colors::border())
                    .bg(colors::BACKGROUND)
                    .text_color(colors::foreground())
                    .overflow_hidden()
                    .on_press(|| {})
                    .child(
                        view()
                            .px_6()
                            .pt_6()
                            .gap_1()
                            .child(
                                text(tr("プロジェクトを編集"))
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .child(
                                text(tr("表示名とセクションはこの端末・接続先ごとに保存されます。フォルダや会話履歴は残ります。"))
                                    .text_size(px(13.0))
                                    .text_color(colors::muted()),
                            ),
                    )
                    .child(
                        view()
                            .px_6()
                            .py_5()
                            .gap_4()
                            .child(
                                view().gap_2().child(label(tr("表示名"))).child(
                                    ui::field()
                                        .auto_focus(true)
                                        .value(move || name.get())
                                        .on_change(move |text: &str| name.set(text.into()))
                                        .on_submit(move |_: &str| save()),
                                ),
                            )
                            .child(
                                view().gap_2().child(label(tr("セクション"))).child(
                                    ui::field()
                                        .value(move || section.get())
                                        .placeholder(tr("空欄で未分類"))
                                        .on_change(move |text: &str| section.set(text.into()))
                                        .on_submit(move |_: &str| save()),
                                ),
                            )
                            .child(when(
                                move || error().is_some(),
                                move || {
                                    text(move || error().unwrap_or_default())
                                        .text_size(px(13.0))
                                        .text_color(rgb(0xff8e86))
                                },
                            )),
                    )
                    .child(
                        view()
                            .flex_row()
                            .justify_end()
                            .gap_2()
                            .px_6()
                            .py_4()
                            .border_t_1()
                            .border_t_color(colors::border())
                            .bg(colors::SURFACE)
                            .child(ui::action_button(
                                tr("キャンセル"),
                                ActionTone::Secondary,
                                cancel,
                            ))
                            .child(ui::action_button(tr("保存"), ActionTone::Primary, save)),
                    ),
            )
            .into_element()
    }
}

fn editor_data(editor: Option<&Editor>) -> Value {
    json!({"projectEditor": {"editor": editor.map(|editor| json!({
        "id": editor.id, "name": editor.name, "section": editor.section, "error": editor.error,
    }))}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_editor_contract_preserves_input_and_validation_without_exposing_routing_state() {
        let editor = Editor {
            id: "fixture-editor".into(),
            scope: ListScope {
                host: "local".into(),
                storage: "account".into(),
            },
            project: "project".into(),
            name: "日本語のプロジェクト 👋".into(),
            section: "個人".into(),
            error: Some("表示名は1〜100文字、セクションは60文字以内で入力してください".into()),
        };
        let expected: Value = serde_json::from_str(include_str!(
            "../apple/Tests/BexNshNativeTests/Fixtures/project-editor.json"
        ))
        .unwrap();
        assert_eq!(editor_data(Some(&editor)), expected);
        assert_eq!(
            editor_data(None),
            json!({"projectEditor": {"editor": null}})
        );
    }
}
