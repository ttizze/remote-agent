use crate::l10n::{localized, tr, tr_format};
use crate::{
    Connection,
    shared::Shared,
    ui::{self, Enabled, Icon, colors},
};
#[cfg(any(desktop, test))]
use agent_core::presentation::diff::WorkspaceDiffSummary;
use agent_core::{
    presentation::{
        agents::AgentSummary,
        diff::{
            DiffCounts, DiffPanelRow, DiffRow, WorkspaceDiffFile, diff_files, diff_panel_file_row,
            diff_panel_row_ends_card, diff_panel_rows, workspace_diff_summary,
        },
    },
    state::{Intent, operations as op},
};
use nsh_core::{
    element::{Rgba, rgb},
    prelude::*,
};
use std::{collections::BTreeSet, sync::Arc};

/// The desktop workspace card's content: the folder, its editors, review and sources.
#[cfg(any(desktop, test))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceSummary {
    pub directory: String,
    pub host: String,
    pub actions: agent_core::presentation::workspace::WorkspaceActions,
    pub enabled: bool,
    pub has_directory: bool,
    pub branch: String,
    pub changes: WorkspaceDiffSummary,
    /// The Host's editors and their labels; only the local Host's open here.
    pub editors: Vec<(agent_protocol::editors::EditorId, String)>,
    pub editor: Option<agent_protocol::editors::EditorId>,
    pub editor_label: String,
    /// Each source's path and file name.
    pub sources: Vec<(String, String)>,
}

#[cfg(any(desktop, test))]
pub(crate) fn workspace_summary(
    p: &agent_core::presentation::view::Presentation,
    local: bool,
) -> WorkspaceSummary {
    use agent_core::presentation::{editors, markdown::file_basename};
    use agent_protocol::editors::FileManagerRevealKind;
    let cwd = p.context.selected_directory();
    let review = p
        .pane()
        .workspace
        .review
        .as_ref()
        .filter(|_| !cwd.is_empty() && p.pane().workspace.review_cwd.as_ref() == Some(&cwd));
    let capabilities = p.snapshot.files.file_open_capabilities.as_ref();
    let available = capabilities
        .map(|c| c.editors.as_slice())
        .unwrap_or_default();
    let editor = editors::preferred_editor(p.snapshot.files.preferred_editor, available);
    let manager = capabilities
        .and_then(|c| c.reveal)
        .unwrap_or(FileManagerRevealKind::Finder);
    let sources = p
        .conversation
        .as_ref()
        .into_iter()
        .flat_map(|c| c.source_paths.iter())
        .chain(p.composer.draft.attachments.iter().map(|a| &a.path))
        .collect::<std::collections::BTreeSet<_>>();
    WorkspaceSummary {
        host: p
            .settings
            .host_name()
            .unwrap_or_else(|| tr("接続先").into()),
        actions: agent_core::presentation::workspace::workspace_actions(&p.snapshot, &p.pane_id),
        directory: if cwd.is_empty() {
            tr("フォルダを選択").into()
        } else {
            file_basename(&cwd).into()
        },
        enabled: p.composer.controls.connected,
        has_directory: !cwd.is_empty(),
        branch: review.map(|r| r.branch.clone()).unwrap_or_default(),
        changes: review
            .map(|r| workspace_diff_summary(r))
            .unwrap_or_default(),
        editors: if local {
            available
                .iter()
                .map(|id| {
                    (
                        *id,
                        editors::editor_label(crate::l10n::language(), *id, manager).to_owned(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        },
        editor,
        editor_label: editor
            .map(|id| {
                tr_format(
                    "{0}で開く",
                    [&editors::editor_label(crate::l10n::language(), id, manager).to_owned()],
                )
            })
            .unwrap_or_else(|| tr("エディターで開く").into()),
        sources: sources
            .into_iter()
            .map(|path| (path.clone(), file_basename(path).into()))
            .collect(),
    }
}

pub(crate) fn diff(cwd: String, host: String) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let cwd = stored(cwd);
    let host = stored(host);
    let review = computed(move || {
        connection.publication.with(|p| {
            let workspace = &p.pane().workspace;
            (workspace.review_cwd.as_ref() == Some(&cwd.get()))
                .then(|| workspace.review.clone().map(Shared))
                .flatten()
        })
    });
    let projected = signal(None::<Result<Shared<Vec<WorkspaceDiffFile>>, String>>);
    effect(move || {
        if let Some(review) = review.get() {
            // Parsing large diffs does not block the UI. The effect's task is
            // cancelled when a newer review or a different surface replaces it.
            // Keep the mounted list until its replacement is ready, including
            // while the workspace review is being fetched.
            spawn(async move {
                projected.set(Some(
                    blocking(move || Shared(Arc::new(diff_files(&review))))
                        .await
                        .map_err(|reason| reason.to_string()),
                ));
            });
        } else {
            projected.set(None);
        }
    });
    let load = move || {
        connection.client.with(|client| {
            client.dispatch(
                &host.get(),
                connection.pane,
                Intent::ReviewWorkspace(op::ReviewWorkspace {
                    base_ref: None,
                    cwd: cwd.get(),
                }),
            )
        })
    };
    effect(load);
    let pending = computed(move || {
        connection.publication.with(|p| {
            p.snapshot
                .operation_running(op::OperationKey::WorkspaceReview {
                    pane: p.pane_id.clone(),
                })
        })
    });
    let failed = computed(move || connection.context.with(|p| p.workspace_error().is_some()));
    let summary = computed(move || {
        review.with(|review| {
            review
                .as_ref()
                .map(|r| workspace_diff_summary(r))
                .unwrap_or_default()
        })
    });
    let files = computed(move || projected.with(|files| files.clone().and_then(Result::ok)));
    let collapsed = signal(BTreeSet::<String>::new());
    let rows = computed(move || {
        files.with(|files| {
            files.as_ref().map_or_else(Vec::new, |files| {
                collapsed.with(|collapsed| diff_panel_rows(files, collapsed))
            })
        })
    });
    let body = computed(move || {
        if failed.get() {
            return DiffBody::Hidden;
        }
        projected.with(|result| match result {
            None => DiffBody::Loading,
            Some(Err(reason)) => DiffBody::Message(localized(reason.clone())),
            Some(Ok(files)) if files.is_empty() => {
                DiffBody::Message(tr("変更はありません").to_owned())
            }
            Some(Ok(_)) => DiffBody::Files,
        })
    });
    let message = move || match body.get() {
        DiffBody::Message(message) => message,
        _ => String::new(),
    };
    let list = element_ref();
    let paths = move || {
        files.with(|files| {
            files
                .as_ref()
                .map(|files| files.iter().map(|file| file.path.clone()).collect())
                .unwrap_or_else(Vec::new)
        })
    };
    let show_file = move |path: String| {
        collapsed.update(|collapsed| {
            collapsed.remove(&path);
        });
        let index = files.with_untracked(|files| {
            rows.with_untracked(|rows| diff_panel_file_row(files.as_ref()?, rows, &path))
        });
        if let Some(index) = index {
            crate::conversation::reveal_row(list, index as u32);
        }
    };
    view()
        .flex_1()
        .min_h_0()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .min_h(px(56.0))
                .py_2()
                .px_3()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    change_heading(move || summary.with(|s| s.comparison.clone()))
                        .flex_1()
                        .min_w_0(),
                )
                .child(comparison_menu(connection, move || cwd.get(), review))
                .child(line_counts(move || summary.with(|s| s.counts)))
                .child(when(
                    move || body.get() == DiffBody::Files,
                    move || {
                        ui::choice_menu(
                            "doc.text",
                            tr("差分のファイルを選択"),
                            move || tr_format("ファイルを選択 ({0})", [&paths().len()]),
                            move || {
                                crate::native::items(
                                    paths().into_iter().map(|path| (path.clone(), path)),
                                )
                            },
                            move |path| show_file(path.to_owned()),
                        )
                    },
                ))
                .child(
                    ui::icon_button(Icon::COPY, tr("差分をコピー"), move || {
                        if let Some(diff) = review.with_untracked(|review| {
                            review.as_ref().map(|review| review.diff.clone())
                        }) {
                            set_clipboard(diff);
                        }
                    })
                    .enabled(move || review.with(Option::is_some)),
                )
                .child(
                    ui::icon_button(Icon::REFRESH, tr("差分を更新"), load).enabled(move || {
                        !pending.get() && connection.settings.with(|p| p.connected())
                    }),
                ),
        )
        .child(when(
            move || matches!(body.get(), DiffBody::Loading | DiffBody::Message(_)),
            move || {
                view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .px_5()
                    .child(when(move || body.get() == DiffBody::Loading, ui::spinner))
                    .child(
                        text(message)
                            .text_size(px(12.0))
                            .text_color(colors::muted()),
                    )
            },
        ))
        .child(when(
            move || body.get() == DiffBody::Files,
            move || {
                virtual_list(
                    move || rows.with(Vec::len),
                    move |at| {
                        diff_row(
                            computed(move || {
                                let (row, ends_card) = rows.with(|rows| {
                                    let row = *rows.get(at.index())?;
                                    Some((row, diff_panel_row_ends_card(rows, at.index())))
                                })?;
                                files.with(|files| {
                                    let file = files.as_ref()?.get(row.file())?;
                                    Some(DisplayRow {
                                        row,
                                        ends_card,
                                        path: file.path.clone(),
                                        counts: file.additions.zip(file.deletions),
                                        line: match row {
                                            DiffPanelRow::Line { line, .. } => {
                                                file.rows.get(line).cloned()
                                            }
                                            _ => None,
                                        },
                                    })
                                })
                            }),
                            move |path| {
                                collapsed.update(|collapsed| {
                                    if !collapsed.remove(&path) {
                                        collapsed.insert(path);
                                    }
                                })
                            },
                        )
                    },
                )
                .keys(move || rows.get())
                .element_ref(&list)
                .estimated_item_size(px(20.0))
                .content_padding(px(12.0))
                .flex_1()
                .min_h_0()
            },
        ))
}

fn comparison_menu(
    connection: Connection,
    cwd: impl Fn() -> String + Copy + 'static,
    review: Memo<Option<Shared<agent_protocol::models::WorkspaceReview>>>,
) -> impl IntoElement {
    ui::icon_button(Icon::CHEVRON_DOWN, tr("比較先を変更"), move || {
        let target = cwd();
        let host = connection.host.get_untracked();
        let selected = connection.publication.with_untracked(|p| p.pane().workspace.comparison.clone());
        let references = review.with_untracked(|review| {
            agent_core::presentation::workspace::comparison_references(
                &review.as_ref().map(|r| r.controls.clone()).unwrap_or_default(), selected.as_deref())
        });
        let mut items = vec![serde_json::json!({"id": "automatic", "title": tr("自動（既定の比較先）"), "selected": selected.is_none()})];
        items.extend(references.iter().enumerate().map(|(i, reference)| serde_json::json!({"id": i.to_string(), "title": reference, "selected": selected.as_ref() == Some(reference)})));
        ui::popup(&serde_json::json!(items), move |id| {
            if connection.host.get_untracked() != host { return; }
            let base_ref = if id == "automatic" { None } else {
                let Some(reference) = id.parse::<usize>().ok().and_then(|i| references.get(i)).cloned() else { return; };
                Some(reference)
            };
            connection.dispatch(Intent::SetWorkspaceComparison { cwd: target.clone(), base_ref });
        });
    }).enabled(move || review.with(|review| review.is_some()) && connection.composer.with(|p| p.controls.connected) && connection.context.with(|p| p.selected_directory() == cwd()))
}

/// What the Diff panel shows under its header: nothing on a workspace error (its
/// notice tells), a spinner before the first diff loads, why there are no files,
/// or the last parsed files while their replacement loads.
#[derive(Clone, PartialEq)]
enum DiffBody {
    Hidden,
    Loading,
    Message(String),
    Files,
}

/// What a row of the Diff panel shows: its place in its file's card, the file's
/// path and line counts, and its line.
#[derive(Clone, PartialEq)]
struct DisplayRow {
    row: DiffPanelRow,
    ends_card: bool,
    path: String,
    counts: Option<(u64, u64)>,
    line: Option<DiffRow>,
}

const ADDED: Rgba = rgb(0x22c55e);
const REMOVED: Rgba = rgb(0xef4444);

/// Added and removed line counts, in the desktop client's colors.
pub(crate) fn line_counts(counts: impl Fn() -> DiffCounts + Copy + 'static) -> View {
    view()
        .flex_row()
        .gap_1()
        .flex_shrink_0()
        .child(when(
            move || counts().additions.is_some(),
            move || {
                text(move || format!("+{}", counts().additions.unwrap_or_default()))
                    .text_size(px(12.0))
                    .text_color(ADDED)
            },
        ))
        .child(when(
            move || counts().deletions.is_some(),
            move || {
                text(move || format!("−{}", counts().deletions.unwrap_or_default()))
                    .text_size(px(12.0))
                    .text_color(REMOVED)
            },
        ))
}

/// The same compact title and comparison in the workspace card and panel.
pub(crate) fn change_heading(comparison: impl Fn() -> Option<String> + Copy + 'static) -> View {
    view()
        .gap(px(2.0))
        .child(
            text(tr("変更"))
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM),
        )
        .child(when(
            move || comparison().is_some(),
            move || {
                text(move || comparison().unwrap_or_default())
                    .text_size(px(11.0))
                    .text_color(colors::t3_muted())
                    .number_of_lines(1)
            },
        ))
}

/// A row of a file's card: the header, which folds the card, a line of a hunk, or
/// the note that the file has no text diff or that the binary file changed.
fn diff_row(row: Memo<Option<DisplayRow>>, toggle: impl Fn(String) + Copy + 'static) -> View {
    let kind = move || row.with(|row| row.as_ref().map(|row| row.row));
    let header = move || matches!(kind(), Some(DiffPanelRow::File { .. }));
    let collapsed = move || {
        matches!(
            kind(),
            Some(DiffPanelRow::File {
                collapsed: true,
                ..
            })
        )
    };
    let path = move || row.with(|row| row.as_ref().map(|row| row.path.clone()).unwrap_or_default());
    let counts = move || row.with(|row| row.as_ref().and_then(|row| row.counts));
    let line = computed(move || row.with(|row| row.as_ref().and_then(|row| row.line.clone())));
    view()
        .w_full()
        .child(when(
            move || header() && kind().is_some_and(|row| row.file() > 0),
            || view().h(px(12.0)),
        ))
        .child(
            view()
                .border_l_1()
                .border_r_1()
                .border_color(colors::border())
                .bg(colors::SURFACE)
                .overflow_hidden()
                .style_when(header, |s| s.border_t_1().rounded_t(px(ui::RADIUS)))
                .style_when(
                    move || row.with(|row| row.as_ref().is_some_and(|row| row.ends_card)),
                    |s| s.border_b_1().rounded_b(px(ui::RADIUS)),
                )
                .child(when(header, move || {
                    nsh_base::button()
                        .accessibility_label(nsh_core::element::Prop::from(
                            (move || Some(SharedString::from(path()))).into_reactive(),
                        ))
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .h(px(32.0))
                        .px_2()
                        .bg(colors::RAISED)
                        .hover(|s| s.bg(colors::SELECTED))
                        .child(
                            view()
                                .text_color(colors::muted())
                                .child(when(collapsed, || ui::icon(Icon::CHEVRON_RIGHT, 14.0)))
                                .child(when(
                                    move || !collapsed(),
                                    || ui::icon(Icon::CHEVRON_DOWN, 14.0),
                                )),
                        )
                        .child(
                            text(path)
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .number_of_lines(1)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(when(
                            move || counts().is_some(),
                            move || {
                                line_counts(move || {
                                    let (additions, deletions) = counts().unwrap_or_default();
                                    DiffCounts::new(additions, deletions)
                                })
                            },
                        ))
                        .on_press(move || toggle(path()))
                }))
                .child(when(
                    move || {
                        matches!(
                            kind(),
                            Some(DiffPanelRow::NoTextDiff { .. } | DiffPanelRow::Binary { .. })
                        )
                    },
                    move || {
                        text(move || {
                            if matches!(kind(), Some(DiffPanelRow::Binary { .. })) {
                                tr("バイナリファイルが変更されました")
                            } else {
                                tr("テキスト差分はありません")
                            }
                        })
                        .text_size(px(12.0))
                        .text_color(colors::muted())
                        .p_3()
                    },
                ))
                .child(when(
                    move || line.with(Option::is_some),
                    move || diff_line(line),
                )),
        )
}

/// A line of a hunk: its number and text, in the desktop client's tints for what
/// happened to it.
fn diff_line(line: Memo<Option<DiffRow>>) -> View {
    let tint = move || {
        line.with(|line| match line.as_ref().map(|line| line.kind.as_str()) {
            Some("+") => (rgb(0x14291d), rgb(0x9be9a8)),
            Some("-") => (rgb(0x321c20), rgb(0xffa2a2)),
            Some("@") => (rgb(0x232b38), rgb(0x9cc7f5)),
            _ => (colors::SURFACE, rgb(0xd0d0d0)),
        })
    };
    let code = |content: Text| {
        content
            .font_family(SharedString::from("monospace"))
            .text_size(px(12.0))
            .line_height(px(20.0))
    };
    view()
        .flex_row()
        .items_start()
        .bg(move || tint().0)
        .child(
            code(text(move || {
                line.with(|line| {
                    line.as_ref()
                        .and_then(DiffRow::line_number)
                        .map(|number| number.to_string())
                        .unwrap_or_default()
                })
            }))
            .w(px(48.0))
            .flex_shrink_0()
            .pr_2()
            .text_right()
            .text_color(rgb(0x8b8b8b)),
        )
        .child(
            code(text(move || {
                line.with(|line| {
                    line.as_ref()
                        .map(|line| line.text.clone())
                        .unwrap_or_default()
                })
            }))
            .flex_1()
            .min_w_0()
            .pr_2()
            .text_color(move || tint().1)
            .selectable(true),
        )
}

pub(crate) fn agents(host: String) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let thread_id = connection
        .context
        .with_untracked(|p| p.navigation().thread_id);
    connection
        .client
        .with(|client| client.dispatch(&host, None, Intent::WatchAgents { thread_id }));
    // The cleanup owns `host`: the view's stored values are released before it runs.
    cleanup(move || {
        connection
            .client
            .with(|client| client.dispatch(&host, None, Intent::WatchAgents { thread_id: None }))
    });
    let agents = computed(move || connection.publication.with(|p| Shared(p.agents.clone())));
    agents_panel(agents)
}

/// The desktop client's agents panel: a heading with how many work, then each
/// agent indented under the one that started it.
fn agents_panel(agents: Memo<Shared<Vec<AgentSummary>>>) -> impl IntoElement {
    use crate::ui::{self, Icon, colors};
    view()
        .flex_1()
        .min_h_0()
        .gap_3()
        .p_4()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(ui::icon(Icon::NETWORK, 16.0))
                .child(
                    text(tr("エージェント"))
                        .flex_1()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    text(move || {
                        let working = agents.with(|rows| rows.iter().filter(|a| a.active).count());
                        tr_format("{0} 件が実行中", [&working])
                    })
                    .text_size(px(13.0))
                    .text_color(colors::muted()),
                ),
        )
        .child(when(
            move || agents.with(|rows| rows.is_empty()),
            || {
                text(tr("サブエージェントが起動するとここに表示されます。"))
                    .text_size(px(13.0))
                    .text_color(colors::muted())
            },
        ))
        .child(scroll().flex_1().min_h_0().child(view().gap_2().child(each(
            move || agents.with(|rows| rows.to_vec()),
            |agent: &AgentSummary| agent.key.clone(),
            |agent, _| agent_row(agent),
        ))))
}

/// An agent: its title, activity and status, then what it is doing and its model.
fn agent_row(agent: ReadSignal<AgentSummary>) -> impl IntoElement {
    use crate::ui::{self, colors};
    let field = move |read: fn(&AgentSummary) -> Option<String>| {
        move || agent.with(|a| read(a).unwrap_or_default())
    };
    view()
        .ml(move || px(agent.with(|a| a.depth) as f32 * 16.0))
        .gap_1()
        .p_3()
        .rounded(px(8.0))
        .border_1()
        .border_color(colors::border())
        .bg(colors::RAISED)
        .child(
            view()
                .flex_row()
                .items_center()
                .h(px(20.0))
                .gap_2()
                .child(when(move || agent.with(|a| a.active), ui::spinner))
                .child(
                    text(move || agent.with(|a| a.title.clone()))
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .number_of_lines(1),
                )
                .child(when(
                    move || agent.with(|a| a.unread),
                    || {
                        view()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded(px(3.0))
                            .bg(colors::foreground())
                    },
                ))
                .child(
                    text(move || agent.with(|a| a.status.clone()))
                        .text_size(px(12.0))
                        .text_color(colors::muted()),
                ),
        )
        .child(
            text(field(|a| a.detail.clone().map(localized)))
                .h(px(20.0))
                .text_size(px(13.0))
                .text_color(colors::muted())
                .number_of_lines(1),
        )
        .child(
            text(field(|a| a.model.clone()))
                .h(px(16.0))
                .text_size(px(12.0))
                .text_color(colors::muted())
                .number_of_lines(1),
        )
}

#[cfg(test)]
mod refresh_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_contract_uses_host_editor_labels_and_preserves_sources() {
        use agent_core::{
            presentation::view::Presentation,
            state::{Attachment, Draft, Snapshot},
        };
        use agent_protocol::{
            editors::{EditorId, FileManagerRevealKind, FileOpenCapabilities},
            models::WorkspaceReview,
        };
        use std::sync::Arc;
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        let pane = Arc::make_mut(snapshot.panes.primary_mut());
        Arc::make_mut(&mut pane.navigation).cwd = "/fixture/日本語 👋".into();
        Arc::make_mut(&mut pane.workspace).review_cwd = Some(pane.navigation.cwd.clone());
        Arc::make_mut(&mut pane.workspace).review = Some(Arc::new(WorkspaceReview {
            controls: Default::default(),
            options: None,
            branch: "feature/native".into(),
            base_ref: Some("origin/main".into()),
            additions: 7,
            deletions: 2,
            files: Vec::new(),
            diff: String::new(),
        }));
        snapshot.files.file_open_capabilities = Some(Arc::new(FileOpenCapabilities {
            editors: vec![EditorId::Cursor, EditorId::FileManager],
            reveal: Some(FileManagerRevealKind::Finder),
        }));
        snapshot.files.preferred_editor = Some(EditorId::Cursor);
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            pane.navigation.draft_key.clone(),
            Arc::new(Draft {
                attachments: vec![Attachment {
                    path: "/fixture/日本語.txt".into(),
                    name: "日本語.txt".into(),
                    is_image: false,
                }],
                ..Default::default()
            }),
        );
        let p = Presentation::project(Arc::new(snapshot), "", None).unwrap();
        assert_eq!(
            workspace_summary(&p, true),
            WorkspaceSummary {
                host: "接続先".into(),
                actions: agent_core::presentation::workspace::WorkspaceActions {
                    can_change: true,
                    ..Default::default()
                },
                directory: "日本語 👋".into(),
                enabled: true,
                has_directory: true,
                branch: "feature/native".into(),
                changes: WorkspaceDiffSummary {
                    comparison: Some("vs origin/main".into()),
                    counts: DiffCounts::new(7, 2),
                },
                editors: vec![
                    (EditorId::Cursor, "Cursor".into()),
                    (EditorId::FileManager, "Finder".into()),
                ],
                editor: Some(EditorId::Cursor),
                editor_label: "Cursorで開く".into(),
                sources: vec![("/fixture/日本語.txt".into(), "日本語.txt".into())],
            }
        );
        assert!(workspace_summary(&p, false).editors.is_empty());
    }

    #[test]
    fn unselected_project_never_exposes_execution_folder_review_or_scripts() {
        use agent_core::{presentation::view::Presentation, state::Snapshot};
        use agent_protocol::{
            models::{
                ProjectMembership, ProjectScript, Thread, WorkspaceControls, WorkspaceReview,
            },
            session::{ProviderKind, SessionRef},
        };
        for cwd in ["", "/execution"] {
            let mut snapshot = Snapshot::default();
            snapshot.connection.connected = true;
            let id = SessionRef::new(ProviderKind::Codex, "no-project".into()).unwrap();
            let pane = Arc::make_mut(snapshot.panes.primary_mut());
            Arc::make_mut(&mut pane.navigation).cwd = cwd.into();
            Arc::make_mut(&mut pane.navigation).thread_id = Some(id.clone());
            let workspace = Arc::make_mut(&mut pane.workspace);
            workspace.review_cwd = Some(cwd.into());
            workspace.review = Some(Arc::new(WorkspaceReview {
                options: None,
                branch: "hidden".into(),
                base_ref: Some("main".into()),
                additions: 3,
                deletions: 1,
                controls: WorkspaceControls {
                    scripts: vec![ProjectScript {
                        name: "hidden".into(),
                        command: "echo hidden".into(),
                    }],
                    ..Default::default()
                },
                files: Vec::new(),
                diff: String::new(),
            }));
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                id.clone(),
                Arc::new(Thread {
                    id: Some(id),
                    cwd: Some(cwd.into()),
                    project_id: ProjectMembership::Unassigned {},
                    ..Default::default()
                }),
            );
            let p = Presentation::project(Arc::new(snapshot), "", None).unwrap();
            let summary = workspace_summary(&p, true);
            assert!(!summary.has_directory);
            assert_eq!(summary.directory, tr("フォルダを選択"));
            assert!(summary.branch.is_empty());
            assert_eq!(summary.changes, Default::default());
            assert_eq!(summary.actions, Default::default());
        }
    }
}
