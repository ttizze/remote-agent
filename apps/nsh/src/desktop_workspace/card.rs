use super::*;
use crate::ui::{self, Enabled, Icon, colors};

pub(super) fn summary_card(
    summary: Memo<WorkspaceSummary>,
    act: impl Fn(Action) + Copy + 'static,
) -> impl IntoElement {
    view()
        .py_2()
        .text_color(colors::t3_foreground().opacity(0.8))
        .child(host_row(summary, act))
        .child(when(
            move || summary.with(|s| s.has_directory),
            move || {
                view()
                    .child(editor_row(summary, act))
                    .child(scripts(summary, act))
                    .child(
                        row(Icon::PLUS, text(tr("プロジェクトスクリプトを追加")))
                            .mx_2()
                            .on_press(move || act(Action::AddScript))
                            .disabled(move || !summary.with(|s| s.actions.can_change)),
                    )
            },
        ))
        .child(when(
            move || summary.with(|s| s.has_directory && !s.branch.is_empty()),
            move || version_control(summary, act),
        ))
}

fn version_control(
    summary: Memo<WorkspaceSummary>,
    act: impl Fn(Action) + Copy + 'static,
) -> impl IntoElement {
    view()
        .child(separator())
        .child(
            row(
                Icon::GIT_BRANCH,
                text(move || summary.with(|s| s.branch.clone())).test_id("thread-details-branch"),
            )
            .mx_2()
            .child(ui::icon(Icon::CHEVRON_DOWN, 14.0))
            .on_press(move || act(Action::Branch))
            .disabled(move || !summary.with(|s| s.actions.can_change)),
        )
        .child(
            row(
                Icon::PUSH,
                text(tr("プッシュ")).test_id("thread-details-push"),
            )
            .mx_2()
            .child(ui::icon(Icon::CHEVRON_DOWN, 14.0))
            .on_press(move || act(Action::Push))
            .disabled(move || !summary.with(|s| s.actions.can_push)),
        )
        .child(changes_row(summary, act).disabled(move || !summary.with(|s| s.enabled)))
}

fn host_row(
    summary: Memo<WorkspaceSummary>,
    act: impl Fn(Action) + Copy + 'static,
) -> impl IntoElement {
    view()
        .mx_2()
        .flex_row()
        .items_center()
        .child(
            row(Icon::SERVER, text(move || summary.with(|s| s.host.clone())))
                .flex_1()
                .min_w_0()
                .text_color(colors::t3_muted())
                .on_press(move || act(Action::Files))
                .disabled(move || !summary.with(|s| s.enabled)),
        )
        .child(
            ui::icon_button(
                Icon::FOLDER_OPEN,
                tr("ワークスペースの操作"),
                move || {
                    let sources = summary.with_untracked(|s| s.sources.clone());
                    let mut items = vec![
                        json!({"id":"files","title":tr("すべてのファイル")}),
                        json!({"id":"attach","title":tr("ソースを追加")}),
                        json!({"id":"refresh","title":tr("変更を更新"),"enabled":summary.with_untracked(|s| s.has_directory)}),
                    ];
                    if !sources.is_empty() {
                        items.push(json!({"separator":true}));
                    }
                    items.extend(
                        sources
                            .iter()
                            .enumerate()
                            .map(|(i, (_, title))| json!({"id":i.to_string(),"title":title})),
                    );
                    ui::popup(&json!(items), move |id| match id {
                        "files" => act(Action::Files),
                        "attach" => act(Action::Attach),
                        "refresh" => act(Action::Refresh),
                        _ => {
                            if let Some((path, _)) =
                                id.parse::<usize>().ok().and_then(|i| sources.get(i))
                            {
                                act(Action::Source(path.clone()));
                            }
                        }
                    });
                },
            )
            .w(px(32.0))
            .h(px(32.0))
            .enabled(move || summary.with(|s| s.enabled)),
        )
}

fn editor_row(
    summary: Memo<WorkspaceSummary>,
    act: impl Fn(Action) + Copy + 'static,
) -> impl IntoElement {
    when(
        move || summary.with(|s| !s.editors.is_empty()),
        move || {
            view().mx_2().flex_row().items_center().rounded(px(8.0)).bg(nsh_core::element::rgba(0xffffff0c))
            .child(row(Icon::EXTERNAL_LINK, text(move || summary.with(|s| s.editor_label.clone())).test_id("thread-details-editor"))
                .flex_1().min_w_0().on_press(move || { if let Some(editor) = summary.with_untracked(|s| s.editor) { act(Action::OpenEditor(editor)); } })
                .disabled(move || !summary.with(|s| s.has_directory && s.editor.is_some())))
            .child(ui::icon_button(Icon::CHEVRON_DOWN, tr("エディターを選択"), move || {
                let items = summary.with_untracked(|s| s.editors.iter().map(|(id,title)| json!({"id":id,"title":title,"selected":Some(*id)==s.editor})).collect::<Vec<_>>());
                ui::popup(&json!(items), move |id| { if let Ok(editor) = serde_json::from_value(json!(id)) { act(Action::PreferEditor(editor)); } });
            }).w(px(32.0)).h(px(32.0)).border_l_1().border_color(colors::line(0x292929)).enabled(move || summary.with(|s| s.has_directory)))
        },
    )
}

fn scripts(
    summary: Memo<WorkspaceSummary>,
    act: impl Fn(Action) + Copy + 'static,
) -> impl IntoElement {
    each(
        move || summary.with(|s| s.actions.controls.scripts.clone()),
        |script| script.name.clone(),
        move |script, _| {
            view()
                .mx_2()
                .flex_row()
                .items_center()
                .child(
                    row(Icon::TERMINAL, text(move || script.get().name))
                        .flex_1()
                        .min_w_0()
                        .on_press(move || act(Action::RunScript(script.get_untracked().command)))
                        .disabled(move || !summary.with(|s| s.enabled && s.has_directory)),
                )
                .child(
                    ui::icon_button(Icon::ELLIPSIS, tr("スクリプトの操作"), move || {
                        act(Action::ManageScript(script.get_untracked()))
                    })
                    .w(px(32.0))
                    .h(px(32.0))
                    .enabled(move || summary.with(|s| s.actions.can_change)),
                )
        },
    )
}

fn changes_row(
    summary: Memo<WorkspaceSummary>,
    act: impl Fn(Action) + Copy + 'static,
) -> nsh_base::ButtonElement {
    nsh_base::button()
        .mx_2()
        .h(px(32.0))
        .px(px(10.0))
        .gap(px(10.0))
        .flex_row()
        .items_center()
        .rounded(px(8.0))
        .hover(|s| s.bg(nsh_core::element::rgba(0xffffff14)))
        .disabled_style(|s| s.opacity(0.64))
        .accessibility_label(tr("変更を表示"))
        .child(
            view()
                .text_color(colors::t3_muted())
                .child(ui::icon(Icon::DIFF, 16.0)),
        )
        .child(
            crate::details::change_heading(move || summary.with(|s| s.changes.comparison.clone()))
                .test_id("thread-details-changes")
                .flex_1()
                .min_w_0(),
        )
        .child(crate::details::line_counts(move || {
            summary.with(|s| s.changes.counts)
        }))
        .on_press(move || act(Action::Diff))
}
