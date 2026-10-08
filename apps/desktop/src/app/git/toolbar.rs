use super::control::{action_intent, pull_intent};
use crate::app::{Desktop, color, ui::icon};
use agent_core::{
    state::{Intent, RefScope},
    view::git::{
        GitQuickActionKind, branch_label, change_request_terminology, default_branch_action_copy,
        pull_label, requires_default_branch_confirmation, resolve_quick_action,
    },
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
    },
    *,
};

/// Renders the compact branch and quick-action controls in the chat header.
pub(in crate::app) fn render(view: &Desktop, cx: &mut Context<Desktop>) -> AnyElement {
    let cwd = view.snapshot.cwd();
    let status = view.snapshot.git.status.get(&cwd);
    let busy = view.snapshot.git.actions.values().any(|event| {
        event.cwd == cwd
            && !matches!(
                &event.kind,
                agent_protocol::vcs::ActionProgressKind::ActionFinished { .. }
                    | agent_protocol::vcs::ActionProgressKind::ActionFailed { .. }
            )
    });
    let quick = resolve_quick_action(
        status,
        busy,
        status.is_some_and(|status| status.is_default_ref),
        status.is_some_and(|status| status.has_primary_remote),
    );
    let branch = branch_label(status);
    let pull = pull_label(status);
    let not_repo = status.is_some_and(|status| !status.is_repo);
    let quick_label = quick.label.clone();
    let quick_kind = quick.kind;
    let quick_action = quick.action;
    let pull_request_url = status.and_then(|status| status.pr.as_ref().map(|pr| pr.url.clone()));
    let default_branch_copy = quick_action.and_then(|action| {
        let status = status?;
        let branch = status.ref_name.as_deref()?;
        requires_default_branch_confirmation(action, status.is_default_ref).then(|| {
            default_branch_action_copy(
                action,
                branch,
                matches!(
                    action,
                    agent_core::view::git::GitAction::CommitPush
                        | agent_core::view::git::GitAction::CommitPushPr
                ),
                Some(change_request_terminology(
                    status
                        .source_control_provider
                        .as_ref()
                        .map(|provider| provider.kind),
                )),
            )
        })
    });
    let disabled = quick.disabled;
    let action_cwd = cwd.clone();
    let refs = view
        .snapshot
        .sources
        .refs(&cwd, RefScope::All)
        .and_then(|entry| entry.list.as_ref())
        .map(|list| list.refs.clone())
        .unwrap_or_default();
    let branch_owner = cx.entity().downgrade();
    let project_id = view.snapshot.selected_project.clone();
    let thread_id = view
        .snapshot
        .selected_thread
        .as_ref()
        .map(ToString::to_string);
    h_flex()
        .gap_1()
        .child(
            Button::new("git-branch")
                .icon(icon("git-branch"))
                .label(branch)
                .ghost()
                .small()
                .tooltip(pull)
                .disabled(status.is_none())
                .dropdown_menu(move |mut menu, _, _| {
                    let refresh_owner = branch_owner.clone();
                    let refresh_cwd = cwd.clone();
                    menu = menu.item(PopupMenuItem::new("Refresh branches").on_click(
                        move |_, _, cx| {
                            let cwd = refresh_cwd.clone();
                            let _ = refresh_owner.update(cx, |view, _| {
                                view.perform(Intent::LoadVcsRefs {
                                    cwd,
                                    query: String::new(),
                                });
                            });
                        },
                    ));
                    if not_repo {
                        let init_owner = branch_owner.clone();
                        let init_cwd = cwd.clone();
                        menu = menu.item(PopupMenuItem::new("Initialize repository").on_click(
                            move |_, _, cx| {
                                let cwd = init_cwd.clone();
                                let _ = init_owner.update(cx, |view, _| {
                                    view.perform(Intent::InitRepository { cwd });
                                });
                            },
                        ));
                    }
                    for reference in refs {
                        let owner = branch_owner.clone();
                        let cwd = cwd.clone();
                        let name = reference.name.clone();
                        let checked = reference.current;
                        let disabled = reference.is_remote || reference.worktree_path.is_some();
                        menu = menu.item(
                            PopupMenuItem::new(name.clone())
                                .checked(checked)
                                .disabled(disabled)
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |view, _| {
                                        view.perform(Intent::SwitchVcsRef {
                                            cwd: cwd.clone(),
                                            ref_name: name.clone(),
                                        });
                                    });
                                }),
                        );
                    }
                    menu
                }),
        )
        .child(
            Button::new("git-action")
                .icon(icon("git-pull-request"))
                .label(quick_label)
                .ghost()
                .small()
                .text_color(if disabled {
                    color("textMuted")
                } else {
                    color("text")
                })
                .disabled(disabled)
                .on_click(cx.listener(move |view, _, window, cx| {
                    let intent = match quick_kind {
                        GitQuickActionKind::RunAction => quick_action.map(|action| {
                            action_intent(
                                action_cwd.clone(),
                                action,
                                project_id.clone(),
                                thread_id.clone(),
                            )
                        }),
                        GitQuickActionKind::Pull => Some(pull_intent(action_cwd.clone())),
                        GitQuickActionKind::OpenPr
                        | GitQuickActionKind::OpenPublish
                        | GitQuickActionKind::Hint => None,
                    };
                    if matches!(quick_kind, GitQuickActionKind::OpenPr) {
                        if let Some(url) = pull_request_url.as_deref() {
                            cx.open_url(url);
                        }
                        return;
                    }
                    if matches!(quick_kind, GitQuickActionKind::OpenPublish) {
                        super::publish::open(action_cwd.clone(), window, cx);
                        return;
                    }
                    if let Some(intent) = intent {
                        if let Some(copy) = default_branch_copy.clone() {
                            view.confirm(
                                crate::app::dialogs::Confirm {
                                    title: Some(copy.title),
                                    message: copy.description,
                                    action: copy.continue_label,
                                    destructive: false,
                                },
                                window,
                                cx,
                                move |view, _, _| view.perform(intent.clone()),
                            );
                        } else {
                            view.perform(intent);
                        }
                    }
                })),
        )
        .into_any_element()
}
