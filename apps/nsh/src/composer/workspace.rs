use super::*;
use crate::{Connection, l10n::tr, ui};
use agent_protocol::workspace::WorkspaceStrategy;
use serde_json::json;

pub(super) fn controls() -> View {
    let connection = use_context::<Connection>().unwrap();
    let selection = computed(move || {
        connection
            .composer
            .with(|composer| composer.controls.workspace.clone())
    });
    let select = move |workspace| {
        connection.dispatch(Intent::SelectWorkspace {
            thread_id: connection
                .composer
                .with_untracked(|composer| composer.controls.navigation.draft_key.clone()),
            workspace: Some(workspace),
        })
    };
    view().child(when(move || selection.with(Option::is_some), move || {
        view().flex_row().flex_wrap().items_center().gap_2().px_2().py_1().text_color(ui::colors::muted()).text_size(px(12.))
            .child(nsh_base::button().on_press(move || {
                let Some(current) = selection.get_untracked() else { return };
                let mut items = vec![json!({"id":"root", "title":tr("ローカル"), "selected":current.mode == "root"})];
                if current.is_repository {
                    items.push(json!({"id":"worktree", "title":tr("ワークツリー"), "selected":current.mode == "worktree"}));
                    for path in &current.worktrees { items.push(json!({"id":format!("existing:{path}"), "title":path, "selected":current.existing_path == *path})); }
                }
                ui::popup(&json!(items), move |id| {
                    let workspace = if id == "worktree" { WorkspaceStrategy::Worktree { base_ref: current.base_ref.clone(), start_from_origin: current.start_from_origin } }
                    else if let Some(path) = id.strip_prefix("existing:") { WorkspaceStrategy::ExistingWorktree { path: path.into() } }
                    else { WorkspaceStrategy::Root };
                    select(workspace);
                });
            }).child(text(move || selection.with(|selection| selection.as_ref().map(|selection| selection.label.clone()).unwrap_or_default()))))
            .child(when(move || selection.with(|selection| selection.as_ref().is_some_and(|selection| selection.mode == "worktree")), move || {
                view().flex_row().items_center().gap_2()
                    .child(input().placeholder(tr("ベースブランチ")).label(tr("ベースブランチ")).auto_correct(false).auto_capitalize(AutoCapitalize::None)
                        .value(move || selection.with(|selection| selection.as_ref().map(|selection| selection.base_ref.clone()).unwrap_or_default()))
                        .on_change(move |value: &str| { if let Some(current) = selection.get_untracked() { select(WorkspaceStrategy::Worktree { base_ref: value.into(), start_from_origin: current.start_from_origin }); } }).w(px(160.)).text_size(px(12.)))
                    .child(nsh_base::button().on_press(move || {
                        let Some(current) = selection.get_untracked() else { return };
                        let items = current.refs.iter().map(|reference| json!({"id":reference,"title":reference,"selected":*reference == current.base_ref})).collect::<Vec<_>>();
                        ui::popup(&json!(items), move |base| select(WorkspaceStrategy::Worktree { base_ref: base.into(), start_from_origin: current.start_from_origin }));
                    }).child(ui::icon(ui::Icon::CHEVRON_DOWN, 12.)))
                    .child(nsh_base::button().on_press(move || { if let Some(current) = selection.get_untracked() { select(WorkspaceStrategy::Worktree { base_ref: current.base_ref, start_from_origin: !current.start_from_origin }); } })
                        .child(text(move || selection.with(|selection| format!("{} {}", if selection.as_ref().is_some_and(|selection| selection.start_from_origin) { "✓" } else { "○" }, tr("リモートから開始"))))))
            }))
    }))
}
