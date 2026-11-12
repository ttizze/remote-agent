use super::*;
use crate::{
    l10n::localized,
    ui::{self, Enabled, colors},
};
use agent_core::presentation::conversation::ActionTone;
use agent_protocol::{
    models::ProjectScript,
    operations::{ChangeWorkspace, WorkspaceAction},
};

#[derive(Clone, PartialEq)]
struct Target {
    host: String,
    cwd: String,
    branch: String,
}
#[derive(Clone, PartialEq)]
enum Form {
    Branch,
    Script(Option<ProjectScript>),
}
#[derive(Clone, Copy)]
pub(super) struct Actions {
    connection: Connection,
    summary: Memo<WorkspaceSummary>,
    form: Signal<Option<(Target, Form)>>,
    busy: Signal<bool>,
    failure: Signal<String>,
    owner: Owner,
}
impl Actions {
    pub fn new(connection: Connection, summary: Memo<WorkspaceSummary>) -> Self {
        Self {
            connection,
            summary,
            form: signal(None),
            busy: signal(false),
            failure: signal(String::new()),
            owner: Owner::current(),
        }
    }
    fn target(self) -> Target {
        Target {
            host: self.connection.host.get_untracked(),
            cwd: self
                .connection
                .context
                .with_untracked(|p| p.selected_directory()),
            branch: self.summary.with_untracked(|s| s.branch.clone()),
        }
    }
    fn open(self, target: Target, form: Form) {
        if self.busy.get_untracked() || !self.summary.with_untracked(|s| s.actions.can_change) {
            return;
        }
        self.failure.set(String::new());
        if target == self.target() {
            self.form.set(Some((target, form)));
        }
    }
    pub fn add_script(self) {
        self.open(self.target(), Form::Script(None));
    }
    pub fn branch(self) {
        let target = self.target();
        let branches = self
            .summary
            .with_untracked(|s| s.actions.controls.branches.clone());
        let mut items = branches.iter().enumerate().map(|(i, name)| json!({"id":i.to_string(),"title":name,"selected":name == &target.branch})).collect::<Vec<_>>();
        items.push(json!({"separator":true}));
        items.push(json!({"id":"create","title":tr("ブランチを作成…")}));
        ui::popup(&json!(items), move |id| {
            if id == "create" {
                self.open(target.clone(), Form::Branch);
                return;
            }
            if let Some(name) = id
                .parse::<usize>()
                .ok()
                .and_then(|i| branches.get(i))
                .filter(|name| *name != &target.branch)
            {
                self.execute(
                    target.clone(),
                    WorkspaceAction::SwitchBranch { name: name.clone() },
                    false,
                );
            }
        });
    }
    pub fn push(self) {
        let target = self.target();
        let remotes = self
            .summary
            .with_untracked(|s| s.actions.controls.remotes.clone());
        let items = remotes
            .iter()
            .enumerate()
            .map(|(i, name)| json!({"id":i.to_string(),"title":name}))
            .collect::<Vec<_>>();
        ui::popup(&json!(items), move |id| {
            if let Some(remote) = id.parse::<usize>().ok().and_then(|i| remotes.get(i)) {
                self.execute(
                    target.clone(),
                    WorkspaceAction::Push {
                        remote: remote.clone(),
                    },
                    false,
                );
            }
        });
    }
    pub fn script_menu(self, script: ProjectScript) {
        let target = self.target();
        ui::popup(
            &json!([{"id":"edit","title":tr("編集")},{"id":"remove","title":tr("削除")}]),
            move |id| match id {
                "edit" => self.open(target.clone(), Form::Script(Some(script.clone()))),
                "remove" => self.execute(
                    target.clone(),
                    WorkspaceAction::RemoveScript {
                        name: script.name.clone(),
                    },
                    false,
                ),
                _ => {}
            },
        );
    }
    fn execute(self, target: Target, action: WorkspaceAction, close: bool) {
        if self.busy.get_untracked() {
            return;
        }
        if target != self.target() {
            self.failed(
                tr("ワークスペースが変わりました。更新して再試行してください。").into(),
                close,
            );
            return;
        }
        self.busy.set(true);
        self.failure.set(String::new());
        let response = self.connection.client.with(|client| {
            client.request(
                &target.host,
                self.connection.pane,
                Intent::ChangeWorkspace(ChangeWorkspace {
                    cwd: target.cwd,
                    branch: target.branch,
                    action,
                }),
            )
        });
        self.owner.run(move || {
            spawn(async move {
                let result = response.recv().await;
                self.busy.set(false);
                match result {
                    Ok(Ok(_)) if close => self.form.set(None),
                    Ok(Ok(_)) => {}
                    Ok(Err(reason)) => self.failed(reason.to_string(), close),
                    Err(reason) => self.failed(reason.to_string(), close),
                }
            })
        });
    }
    fn failed(self, reason: String, in_form: bool) {
        if in_form {
            self.failure.set(localized(reason));
        } else {
            crate::native::message(tr("ワークスペース"), localized(reason));
        }
    }
    pub fn view(self) -> impl IntoElement {
        when(
            move || self.form.with(Option::is_some),
            move || self.editor(),
        )
    }
    fn editor(self) -> impl IntoElement {
        let (target, form) = self.form.get_untracked().unwrap();
        let branch = matches!(form, Form::Branch);
        let existing = match form {
            Form::Script(script) => script,
            _ => None,
        };
        let name = signal(
            existing
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
        );
        let previous_name = stored(existing.as_ref().map(|s| s.name.clone()));
        let command = signal(existing.map(|s| s.command).unwrap_or_default());
        let target = stored(target);
        let valid =
            move || !name.get().trim().is_empty() && (branch || !command.get().trim().is_empty());
        let cancel = move || {
            if !self.busy.get_untracked() {
                self.form.set(None);
            }
        };
        let save = move || {
            if !valid() {
                return;
            }
            let action = if branch {
                WorkspaceAction::CreateBranch {
                    name: name.get_untracked(),
                }
            } else {
                WorkspaceAction::SaveScript {
                    previous_name: previous_name.get(),
                    name: name.get_untracked(),
                    command: command.get_untracked(),
                }
            };
            self.execute(target.get(), action, true);
        };
        nsh_base::dialog()
            .presentation(ModalPresentation::FormSheet)
            .close_on_escape(move || !self.busy.get())
            .close_on_backdrop_press(move || !self.busy.get())
            .on_cancel(cancel)
            .flex_1()
            .items_center()
            .justify_center()
            .child(
                view()
                    .p_5()
                    .gap_3()
                    .w_full()
                    .max_w(px(448.0))
                    .rounded(px(16.0))
                    .bg(colors::SURFACE)
                    .child(
                        text(if branch {
                            tr("ブランチを作成…")
                        } else {
                            tr("プロジェクトスクリプト")
                        })
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        text(move || target.get().cwd)
                            .text_size(px(12.0))
                            .text_color(colors::muted()),
                    )
                    .child(
                        text(if branch {
                            tr("ブランチ名")
                        } else {
                            tr("スクリプト名")
                        })
                        .text_size(px(13.0)),
                    )
                    .child(
                        ui::field()
                            .auto_focus(true)
                            .value(move || name.get())
                            .on_change(move |value: &str| name.set(value.into()))
                            .on_submit(move |_: &str| save())
                            .disabled(self.busy),
                    )
                    .child(when(
                        move || !branch,
                        move || {
                            view()
                                .gap_2()
                                .child(text(tr("コマンド")).text_size(px(13.0)))
                                .child(
                                    ui::field()
                                        .value(move || command.get())
                                        .on_change(move |value: &str| command.set(value.into()))
                                        .on_submit(move |_: &str| save())
                                        .disabled(self.busy),
                                )
                        },
                    ))
                    .child(when(
                        move || !self.failure.get().is_empty(),
                        move || {
                            text(move || self.failure.get())
                                .text_color(crate::conversation::look::DESTRUCTIVE)
                                .selectable(true)
                        },
                    ))
                    .child(
                        view()
                            .flex_row()
                            .justify_end()
                            .gap_2()
                            .child(
                                ui::text_button(tr("キャンセル"), cancel)
                                    .enabled(move || !self.busy.get()),
                            )
                            .child(
                                ui::action_button(
                                    if branch { tr("作成") } else { tr("保存") },
                                    ActionTone::Primary,
                                    save,
                                )
                                .enabled(move || !self.busy.get() && valid()),
                            ),
                    ),
            )
    }
}
