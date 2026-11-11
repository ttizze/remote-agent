//! The worktree settings page: the application owns the editor's lifetime and
//! Core owns its Host operations; this page keeps only the unsaved inputs.
use super::choice;
use crate::Connection;
use crate::application::{WorktreeAction, WorktreeEditor};
use crate::l10n::{localized, tr, tr_format};
use crate::ui::{self, Enabled, colors};
use agent_protocol::models::{Worktree, WorktreeSettings};
use nsh_core::{
    element::{Input, input},
    prelude::*,
};
use serde_json::json;

pub(super) fn page(connection: Connection) -> View {
    let application = connection.application;
    application.with(|app| app.open_worktrees());
    cleanup(move || {
        let token = connection
            .state
            .with_untracked(|state| state.worktrees.as_ref().map(|editor| editor.id.clone()));
        if let Some(token) = token {
            application.with(|app| app.worktree_action(&token, WorktreeAction::Close {}));
        }
    });
    let editor = computed(move || {
        connection.state.with(|state| {
            state
                .worktrees
                .clone()
                .filter(|editor| editor.host == state.host)
        })
    });
    let connected = computed(move || connection.settings.with(|p| p.connected()));
    let inputs = signal(None::<WorktreeSettings>);
    let submission = signal(None::<(String, WorktreeSettings)>);
    let project = signal(None::<String>);
    let emit = move |action: WorktreeAction| {
        if let Some(token) = editor.with_untracked(|editor| editor.as_ref().map(|e| e.id.clone())) {
            application.with(|app| app.worktree_action(&token, action));
        } else if matches!(action, WorktreeAction::Load {}) {
            application.with(|app| app.open_worktrees());
        }
    };
    let busy = move || {
        submission.with(Option::is_some) || editor.with(|e| e.as_ref().is_some_and(|e| e.pending))
    };
    let save = move || {
        let Some(settings) = inputs.get_untracked() else {
            return;
        };
        if submission.with_untracked(Option::is_some)
            || editor.with_untracked(|e| e.as_ref().is_none_or(|e| e.pending))
            || !connected.get_untracked()
        {
            return;
        }
        let id = uuid::Uuid::new_v4().to_string();
        submission.set(Some((id.clone(), settings.clone())));
        emit(WorktreeAction::Save { id, settings });
    };
    // The Host's settings replace the inputs until the user edits them.
    let loaded = computed(move || editor.with(|e| e.as_ref().and_then(|e| e.settings.clone())));
    let previous = stored(None::<WorktreeSettings>);
    effect(move || {
        let current = loaded.get();
        let old = previous.replace(current.clone());
        if current.is_some()
            && submission.with_untracked(Option::is_none)
            && (old.is_none() || inputs.with_untracked(|inputs| *inputs == old))
        {
            inputs.set(current);
        }
    });
    // A saved submission that the user has since changed is saved again.
    let receipt = computed(move || editor.with(|e| e.as_ref().and_then(|e| e.receipt.clone())));
    effect(move || {
        let Some(receipt) = receipt.get() else {
            return;
        };
        let Some((id, sent)) = submission.get_untracked() else {
            return;
        };
        if receipt.id != id {
            return;
        }
        submission.set(None);
        if receipt.succeeded && inputs.with_untracked(|inputs| inputs.as_ref() != Some(&sent)) {
            save();
        }
    });
    let edit = move |change: &dyn Fn(&mut WorktreeSettings)| {
        inputs.update(|inputs| {
            if let Some(inputs) = inputs {
                change(inputs);
            }
        });
        save();
    };
    let enabled = move || inputs.with(Option::is_some) && connected.get();
    view()
        .gap_8()
        .child(scope(connection, project, busy))
        .child(when(
            move || project.with(Option::is_none),
            move || shared(inputs, enabled, edit),
        ))
        .child(when(
            move || project.with(Option::is_some),
            move || project_cleanup(inputs, project, enabled, edit),
        ))
        .child(when(busy, || {
            text(tr("設定を通信中…"))
                .text_size(px(13.0))
                .text_color(colors::muted())
        }))
        .child(when(
            move || !connected.get(),
            || text(tr("作業環境との接続を確認してください。")).text_size(px(13.0)),
        ))
        .child(when(
            move || editor.with(|e| e.as_ref().is_some_and(|e| e.error.is_some())),
            move || failure(editor, busy, connected, emit, save),
        ))
        .child(managed(connection, editor, busy, connected, emit))
}

fn heading(title: &'static str) -> Text {
    text(title)
        .text_size(px(16.0))
        .font_weight(FontWeight::SEMIBOLD)
}

fn note(content: &'static str) -> Text {
    text(content)
        .text_size(px(12.0))
        .text_color(colors::muted())
}

/// Which Host and project the settings apply to; only cleanup varies by project.
fn scope(
    connection: Connection,
    project: Signal<Option<String>>,
    busy: impl Fn() -> bool + Copy + 'static,
) -> View {
    let hosts = computed(move || connection.state.with(|state| state.hosts.clone()));
    let projects = computed(move || {
        connection.publication.with(|p| {
            p.list
                .iter()
                .flat_map(|list| list.projects.iter().chain(&list.hidden_projects))
                .map(|project| (project.id.clone(), project.name.clone()))
                .collect::<Vec<_>>()
        })
    });
    let host_name = move || {
        let host = connection.host.get();
        hosts.with(|hosts| {
            hosts
                .iter()
                .find(|h| h.id == host)
                .map(|h| h.label())
                .unwrap_or_else(|| tr("環境を選択").into())
        })
    };
    let project_name = move || {
        project.with(|selected| {
            selected
                .as_ref()
                .and_then(|id| {
                    projects.with(|all| {
                        all.iter()
                            .find(|(p, _)| p == id)
                            .map(|(_, name)| name.clone())
                    })
                })
                .unwrap_or_else(|| tr("すべてのプロジェクト").into())
        })
    };
    let host_items = move || {
        let host = connection.host.get_untracked();
        json!(hosts.with_untracked(|hosts| {
            hosts
                .iter()
                .map(|h| json!({"id": h.id, "title": h.label(), "selected": h.id == host}))
                .collect::<Vec<_>>()
        }))
    };
    let project_items = move || {
        let selected = project.get_untracked();
        let mut items = vec![
            json!({"id": "", "title": tr("すべてのプロジェクト"), "selected": selected.is_none()}),
        ];
        items.extend(projects.with_untracked(|all| {
            all.iter()
                .map(|(id, name)| json!({"id": id, "title": name, "selected": selected.as_ref() == Some(id)}))
                .collect::<Vec<_>>()
        }));
        json!(items)
    };
    ui::card(vec![ui::setting(
        tr("適用範囲"),
        move || {
            if project.with(Option::is_none) {
                tr("この環境に保存し、新しい会話に適用します。").into()
            } else {
                tr("プロジェクトごとに変えられるのは自動削除だけです。").into()
            }
        },
        view()
            .flex_row()
            .gap_2()
            .child(
                choice(host_name, host_items, move |id: &str| {
                    let id = id.to_owned();
                    let token = connection.state.with_untracked(|state| {
                        state.worktrees.as_ref().map(|editor| editor.id.clone())
                    });
                    if let Some(token) = token {
                        connection
                            .application
                            .with(|app| app.worktree_action(&token, WorktreeAction::Host { id }));
                    }
                })
                .enabled(move || !busy()),
            )
            .child(choice(project_name, project_items, move |id: &str| {
                project.set((!id.is_empty()).then(|| id.to_owned()))
            })),
    )])
}

fn switch(
    label: &'static str,
    value: impl Fn() -> bool + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    change: impl Fn(bool) + 'static,
) -> ui::Switch {
    ui::switch(move || label.into(), value, change).enabled(enabled)
}

fn field(
    placeholder: &'static str,
    value: impl Fn() -> String + 'static,
    enabled: impl Fn() -> bool + 'static,
    change: impl Fn(&str) + 'static,
) -> Input {
    input()
        .auto_correct(false)
        .auto_capitalize(AutoCapitalize::None)
        .placeholder(placeholder)
        .value(value)
        .readonly(move || !enabled())
        .on_change(move |value: &str| change(value))
        .text_color(colors::foreground())
        .w_full()
        .min_h(px(36.0))
        .px_3()
        .py_2()
        .rounded(px(ui::RADIUS))
        .border_1()
        .border_color(colors::border())
}

/// The settings every project shares: creation, copying and cleanup.
fn shared(
    inputs: Signal<Option<WorktreeSettings>>,
    enabled: impl Fn() -> bool + Copy + 'static,
    edit: impl Fn(&dyn Fn(&mut WorktreeSettings)) + Copy + 'static,
) -> View {
    let value = move |read: fn(&WorktreeSettings) -> bool| {
        move || inputs.with(|inputs| inputs.as_ref().is_some_and(read))
    };
    view()
        .gap_4()
        .child(heading(tr("作成と保存先")))
        .child(ui::card(vec![
            ui::setting(
                tr("新規セッションをワークツリーで開始"),
                || tr("元の作業フォルダと分けて、新しい会話を進めます。").into(),
                switch(
                    tr("新規セッションをワークツリーで開始"),
                    value(|s| s.create_on_new_session),
                    enabled,
                    move |on| edit(&|s| s.create_on_new_session = on),
                ),
            ),
            view().px_5().py_4().gap_2().child(field(
                tr("この環境の絶対パス（空欄で既定）"),
                move || inputs.with(|s| s.as_ref().map(|s| s.worktree_directory.clone()).unwrap_or_default()),
                enabled,
                move |text| {
                    let text = text.to_owned();
                    edit(&|s| s.worktree_directory = text.clone())
                },
            )),
        ]))
        .child(note(tr("保存先に「セッション名/リポジトリ名」の構成で作ります。空欄なら元のリポジトリ内の .worktree に保存します。既存のワークツリーは移動しません。")))
        .child(heading(tr("コピー対象")))
        .child(ui::card(vec![
            ui::setting(
                tr("作成時にファイルをコピー"),
                || tr("作成時に、指定したファイルを元のリポジトリからコピーします。").into(),
                switch(
                    tr("作成時にファイルをコピー"),
                    value(|s| s.copy_on_create),
                    enabled,
                    move |on| edit(&|s| s.copy_on_create = on),
                ),
            ),
            view().px_5().py_4().child(
                field(
                    tr("コピー対象"),
                    move || inputs.with(|s| s.as_ref().map(|s| s.copy_paths.join("\n")).unwrap_or_default()),
                    enabled,
                    move |text| {
                        let paths = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
                        edit(&|s| s.copy_paths = paths.clone())
                    },
                )
                .multiline(true)
                // A multiline field shows its placeholder as a separate label,
                // so the field itself is named.
                .label(tr("コピー対象"))
                .min_h(px(88.0)),
            ),
        ]))
        .child(note(tr("リポジトリからの相対パスを1行に1つ指定します（例: .env、config/local）。存在しないパスはスキップします。")))
        .child(heading(tr("自動削除")))
        .child(ui::card(vec![ui::setting(
            tr("取り込み済みを自動削除"),
            || tr("既定ブランチより先のコミットがない作業場所を1時間ごとに削除します。実行中・回答待ち・ターミナル使用中・ローカル変更ありの場合は保留します。ブランチと会話履歴は残ります。").into(),
            switch(
                tr("取り込み済みを自動削除"),
                value(|s| s.delete_unchanged),
                enabled,
                move |on| edit(&|s| s.delete_unchanged = on),
            ),
        )]))
}

/// A project's cleanup: the shared choice, or its own.
fn project_cleanup(
    inputs: Signal<Option<WorktreeSettings>>,
    project: Signal<Option<String>>,
    enabled: impl Fn() -> bool + Copy + 'static,
    edit: impl Fn(&dyn Fn(&mut WorktreeSettings)) + Copy + 'static,
) -> View {
    let current = move || {
        let id = project.get().unwrap_or_default();
        inputs.with(|s| {
            s.as_ref()
                .and_then(|s| s.project_delete_unchanged.get(&id).copied())
        })
    };
    let shared_label = move || {
        if inputs.with(|s| s.as_ref().is_some_and(|s| s.delete_unchanged)) {
            tr("すべてのプロジェクトと同じ（オン）")
        } else {
            tr("すべてのプロジェクトと同じ（オフ）")
        }
    };
    let label = move || match current() {
        None => shared_label().to_owned(),
        Some(true) => tr("オン").to_owned(),
        Some(false) => tr("オフ").to_owned(),
    };
    let items = move || {
        let current = untracked(current);
        json!([
            {"id": "shared", "title": untracked(shared_label), "selected": current.is_none()},
            {"id": "on", "title": tr("オン"), "selected": current == Some(true)},
            {"id": "off", "title": tr("オフ"), "selected": current == Some(false)},
        ])
    };
    view()
        .gap_4()
        .child(heading(tr("自動削除")))
        .child(ui::card(vec![ui::setting(
            tr("取り込み済みを自動削除"),
            || tr("このプロジェクトの作業場所に適用します。").into(),
            choice(label, items, move |id: &str| {
                let Some(project) = project.get_untracked() else {
                    return;
                };
                let value = match id {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                edit(&|s| match value {
                    Some(value) => {
                        s.project_delete_unchanged.insert(project.clone(), value);
                    }
                    None => {
                        s.project_delete_unchanged.remove(&project);
                    }
                })
            })
            .enabled(enabled),
        )]))
}

fn failure(
    editor: Memo<Option<WorktreeEditor>>,
    busy: impl Fn() -> bool + Copy + 'static,
    connected: Memo<bool>,
    emit: impl Fn(WorktreeAction) + Copy + 'static,
    save: impl Fn() + Copy + 'static,
) -> View {
    ui::card(vec![ui::setting(
        tr("保存できませんでした"),
        move || {
            editor.with(|e| {
                e.as_ref()
                    .and_then(|e| e.error.clone())
                    .map(localized)
                    .unwrap_or_default()
            })
        },
        view()
            .flex_row()
            .gap_2()
            .child(
                ui::text_button(tr("再読み込み"), move || emit(WorktreeAction::Load {}))
                    .enabled(move || !busy() && connected.get()),
            )
            .child(ui::text_button(tr("保存を再試行"), save).enabled(move || {
                !busy()
                    && connected.get()
                    && editor.with(|e| e.as_ref().is_some_and(|e| e.settings.is_some()))
            })),
    )])
}

/// The worktrees Bex created, with their conversations and removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedState {
    Loading,
    Disconnected,
    Unavailable,
    Failed,
    Empty,
    Ready,
}

fn managed_state(
    rows: Option<&[Worktree]>,
    editor: bool,
    pending: bool,
    error: bool,
    connected: bool,
) -> ManagedState {
    if !editor {
        return if connected {
            ManagedState::Unavailable
        } else {
            ManagedState::Disconnected
        };
    }
    if let Some(rows) = rows {
        return if rows.is_empty() {
            ManagedState::Empty
        } else {
            ManagedState::Ready
        };
    }
    if error {
        ManagedState::Failed
    } else if !connected {
        ManagedState::Disconnected
    } else if pending {
        ManagedState::Loading
    } else {
        // A cancelled or otherwise lost request must not leave the page in a
        // permanent loading state.  The refresh action is still available.
        ManagedState::Failed
    }
}

fn managed(
    connection: Connection,
    editor: Memo<Option<WorktreeEditor>>,
    busy: impl Fn() -> bool + Copy + 'static,
    connected: Memo<bool>,
    emit: impl Fn(WorktreeAction) + Copy + 'static,
) -> View {
    let rows = computed(move || {
        connection
            .publication
            .with(|p| p.snapshot.files.workspace.worktrees.as_deref().cloned())
    });
    let list_state = computed(move || {
        let editor_present = editor.with(Option::is_some);
        let pending = editor.with(|editor| editor.as_ref().is_some_and(|editor| editor.pending));
        let error =
            editor.with(|editor| editor.as_ref().is_some_and(|editor| editor.error.is_some()));
        let connected = connected.get();
        rows.with(|rows| managed_state(rows.as_deref(), editor_present, pending, error, connected))
    });
    let idle = move || !busy() && connected.get();
    view()
        .gap_3()
        .child(
            view()
                .flex_row()
                .items_center()
                .child(heading(tr("作成済みのワークツリー")).flex_1())
                .child(
                    ui::text_button(tr("ワークツリー一覧を更新"), move || {
                        emit(WorktreeAction::Load {})
                    })
                    .enabled(idle),
                ),
        )
        .child(when(
            move || list_state.get() == ManagedState::Loading,
            || note(tr("一覧を読み込み中…")),
        ))
        .child(when(
            move || list_state.get() == ManagedState::Disconnected,
            || note(tr("作業環境との接続を確認してください。")),
        ))
        .child(when(
            move || list_state.get() == ManagedState::Unavailable,
            || note(tr("ワークツリーを利用できません。再読み込みしてください。")),
        ))
        .child(when(
            move || {
                list_state.get() == ManagedState::Failed
                    && editor
                        .with(|editor| editor.as_ref().is_none_or(|editor| editor.error.is_none()))
            },
            || {
                note(tr(
                    "ワークツリー一覧を読み込めませんでした。再読み込みしてください。",
                ))
            },
        ))
        .child(when(
            move || list_state.get() == ManagedState::Empty,
            || {
                ui::card(vec![
                    view()
                        .px_5()
                        .py_4()
                        .child(note(tr("Bexで作成したワークツリーはありません。"))),
                ])
            },
        ))
        .child(each(
            move || rows.get().unwrap_or_default(),
            |row: &Worktree| row.path.clone(),
            move |row, _| worktree(connection, row, idle, emit),
        ))
}

fn worktree(
    connection: Connection,
    row: ReadSignal<Worktree>,
    idle: impl Fn() -> bool + Copy + 'static,
    emit: impl Fn(WorktreeAction) + Copy + 'static,
) -> View {
    let reason = move || {
        let cwd = connection.context.with(|p| p.selected_directory());
        row.with(|row| {
            agent_core::presentation::worktrees::worktree_removal_reason(
                row.blocked_reason.clone(),
                cwd,
                row.path.clone(),
            )
            .map(localized)
        })
    };
    let remove = move || {
        let path = row.with_untracked(|row| row.path.clone());
        spawn(async move {
            if nsh_base::alert_dialog()
                .title(tr("作業ディレクトリを削除しますか？"))
                .description(tr("この作業ディレクトリを削除します。ブランチと会話履歴は残ります。次のメッセージ送信時に、元プロジェクトの main から同じ場所に作り直します。"))
                .cancel("cancel", tr("キャンセル"))
                .destructive("remove", tr("削除"))
                .show()
                .await
                .is_ok_and(|answer| answer.action.as_deref() == Some("remove"))
            {
                emit(WorktreeAction::Remove { path });
            }
        });
    };
    ui::card(vec![
        view()
            .px_5()
            .py_4()
            .gap_2()
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        text(move || {
                            row.with(|row| {
                                format!(
                                    "{} · {}",
                                    agent_core::presentation::markdown::file_basename(
                                        &row.project_path
                                    ),
                                    row.branch
                                )
                            })
                        })
                        .flex_1()
                        .min_w_0()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        ui::text_button(tr("変更を確認"), move || {
                            emit(WorktreeAction::Review {
                                path: row.with_untracked(|row| row.path.clone()),
                            })
                        })
                        .enabled(idle),
                    )
                    .child(
                        ui::text_button(tr("削除…"), remove)
                            .enabled(move || idle() && reason().is_none()),
                    ),
            )
            .child(
                text(move || row.with(|row| row.path.clone()))
                    .text_size(px(12.0))
                    .text_color(colors::muted())
                    .selectable(true),
            )
            .child(each(
                move || row.with(|row| row.threads.clone()),
                |thread: &agent_protocol::models::WorktreeThread| thread.id.clone(),
                move |thread, _| {
                    let label = move || {
                        thread.with(|t| {
                            SharedString::from(if t.active {
                                tr_format("実行中 · {0}", [&t.name])
                            } else {
                                t.name.clone()
                            })
                        })
                    };
                    ui::text_button(label, move || {
                        emit(WorktreeAction::Open {
                            thread: thread.with_untracked(|t| t.id.clone()),
                        })
                    })
                    .enabled(idle)
                    .self_start()
                },
            ))
            .child(when(
                move || reason().is_some(),
                move || {
                    text(move || reason().unwrap_or_default())
                        .text_size(px(12.0))
                        .text_color(colors::muted())
                },
            )),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_state_does_not_turn_unavailable_or_failed_requests_into_loading() {
        assert_eq!(
            managed_state(None, false, false, false, true),
            ManagedState::Unavailable
        );
        assert_eq!(
            managed_state(None, true, true, false, true),
            ManagedState::Loading
        );
        assert_eq!(
            managed_state(None, true, false, false, false),
            ManagedState::Disconnected
        );
        assert_eq!(
            managed_state(None, true, false, false, true),
            ManagedState::Failed
        );
        assert_eq!(
            managed_state(None, true, false, true, true),
            ManagedState::Failed
        );
        assert_eq!(
            managed_state(Some(&[]), true, true, true, true),
            ManagedState::Empty
        );
    }
}
