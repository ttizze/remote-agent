//! Desktop pull-request surface backed by agent-core views and intents.
use super::PanelTab;
use crate::app::{
    Desktop,
    ui::{color, icon, tint},
};
use agent_core::{
    state::{Intent, PullRequestStackHeadInput, PullRequestViewedFileInput},
    view::pull_requests::PullRequestPanelOptions,
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        scroll::ScrollableElement,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

#[derive(Default)]
pub(super) struct PullRequestsState {
    pub(super) selected: Option<String>,
}

impl Desktop {
    pub(super) fn sync_pull_requests(&mut self) {
        if !self.panel_shows(PanelTab::PullRequests) || !self.snapshot.connected {
            return;
        }
        let Some(project_id) = self.snapshot.selected_project.clone() else {
            return;
        };
        if !self
            .snapshot
            .pull_requests
            .by_project
            .contains_key(&project_id)
        {
            self.perform(Intent::LoadPullRequests {
                project_id,
                repository: None,
                query: None,
                include_closed: false,
            });
        }
    }

    pub(super) fn render_pull_requests(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> AnyElement {
        let project_id = self.snapshot.selected_project.clone().unwrap_or_default();
        let view = self.snapshot.pull_request_list(PullRequestPanelOptions {
            project_id: Some(project_id.clone()),
            query: String::new(),
            include_closed: true,
        });
        let owner = cx.entity().downgrade();
        let rows = view.entries.into_iter().map(|entry| {
            let owner = owner.clone();
            let key = entry.key.clone();
            let label = format!("#{}  {}", key.number, entry.title);
            let project_id = project_id.clone();
            h_flex()
                .id(SharedString::from(format!(
                    "pull-request-{}",
                    key.canonical()
                )))
                .w_full()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(tint("accentSurface", 0.6)))
                .on_click(move |_, _, cx| {
                    let key = key.clone();
                    let project_id = project_id.clone();
                    let _ = owner.update(cx, |view, _| {
                        view.panels.pull_requests.selected = Some(key.canonical());
                        view.perform(Intent::LoadPullRequest {
                            project_id: project_id.clone(),
                            host: Some(key.host.clone()),
                            repository: key.repository.clone(),
                            number: key.number,
                        });
                        view.perform(Intent::LoadPullRequestViewedFiles {
                            project_id: project_id.clone(),
                            host: Some(key.host.clone()),
                            repository: key.repository.clone(),
                            number: key.number,
                        });
                    });
                })
                .child(
                    icon("git-pull-request")
                        .size_4()
                        .text_color(color("textMuted")),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(div().truncate().child(label))
                        .child(
                            div()
                                .text_xs()
                                .text_color(color("textMuted"))
                                .child(format!("{} → {}", entry.head_branch, entry.base_branch)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(format!("{:?}", entry.badge)),
                )
        });
        let refresh_project = project_id.clone();
        let detail = self
            .panels
            .pull_requests
            .selected
            .as_ref()
            .and_then(|key| self.snapshot.pull_requests.details.get(key))
            .cloned();
        let detail_view = detail.map(|detail| {
            let key = detail.summary.key.clone();
            let owner = cx.entity().downgrade();
            let project_id = project_id.clone();
            let diff_view = self.snapshot.pull_request_diff(key.clone());
            let diff_files = diff_view
                .as_ref()
                .map(|diff| diff.files.clone())
                .unwrap_or_default();
            let diff_next_cursor = diff_view.and_then(|diff| diff.next_cursor);
            let viewed_files = self
                .snapshot
                .pull_requests
                .viewed_files
                .get(&key.canonical())
                .map(|viewed| {
                    viewed
                        .files
                        .iter()
                        .map(|file| (file.path.clone(), file.viewed))
                        .collect::<std::collections::BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            let thread_id = self
                .snapshot
                .selected_thread
                .as_ref()
                .map(ToString::to_string);
            let linked = thread_id.as_ref().and_then(|thread| {
                let thread_id = agent_domain::ThreadId::new(thread.clone()).ok()?;
                self.snapshot
                    .pull_requests
                    .links_by_thread
                    .get(&thread_id)
                    .and_then(|links| links.iter().find(|link| link.key() == key))
                    .cloned()
            });
            let action = |label: &'static str, action: &'static str| {
                let owner = owner.clone();
                let key = key.clone();
                let project_id = project_id.clone();
                let stack = detail.summary.stack.clone();
                let stack_number = stack.as_ref().and_then(|stack| {
                    matches!(action, "merge" | "update_branch").then_some(stack.number)
                });
                let expected_stack_heads = stack
                    .as_ref()
                    .filter(|_| matches!(action, "merge" | "update_branch"))
                    .map(|stack| {
                        let end = stack
                            .layers
                            .iter()
                            .position(|layer| layer.number == key.number)
                            .map(|position| position + 1)
                            .unwrap_or(stack.layers.len());
                        stack
                            .layers
                            .iter()
                            .take(if action == "merge" {
                                end
                            } else {
                                stack.layers.len()
                            })
                            .filter(|layer| layer.state != agent_domain::PullRequestState::Merged)
                            .filter_map(|layer| {
                                Some(PullRequestStackHeadInput {
                                    number: layer.number,
                                    head_sha: layer.head_sha.clone()?,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let confirmation = match action {
                    "merge" => Some(("Merge pull request", "Merge this pull request?", true)),
                    "close" => Some(("Close pull request", "Close this pull request?", true)),
                    "update_branch" => Some((
                        "Update pull request branch",
                        "Update this pull request branch?",
                        false,
                    )),
                    _ => None,
                };
                Button::new(SharedString::from(format!("pull-request-action-{action}")))
                    .label(label)
                    .ghost()
                    .xsmall()
                    .on_click(move |_, window, cx| {
                        let intent = Intent::PullRequestAction {
                            project_id: project_id.clone(),
                            host: Some(key.host.clone()),
                            repository: key.repository.clone(),
                            number: key.number,
                            action: action.into(),
                            stack_number,
                            expected_stack_heads: expected_stack_heads.clone(),
                            merge_method: Some("merge".into()),
                        };
                        let _ = owner.update(cx, |view, cx| {
                            if let Some((title, message, destructive)) = confirmation {
                                view.confirm(
                                    crate::app::dialogs::Confirm {
                                        title: Some(title.into()),
                                        message: message.into(),
                                        action: label.into(),
                                        destructive,
                                    },
                                    window,
                                    cx,
                                    move |view, _, _| view.perform(intent.clone()),
                                );
                            } else {
                                view.perform(intent);
                            }
                        });
                    })
            };
            let review = |label: &'static str, verdict: &'static str| {
                let owner = owner.clone();
                let key = key.clone();
                let project_id = project_id.clone();
                Button::new(SharedString::from(format!("pull-request-review-{verdict}")))
                    .label(label)
                    .ghost()
                    .xsmall()
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |view, _| {
                            view.perform(Intent::SubmitPullRequestReview {
                                project_id: project_id.clone(),
                                host: Some(key.host.clone()),
                                repository: key.repository.clone(),
                                number: key.number,
                                verdict: verdict.into(),
                                body: String::new(),
                            });
                        });
                    })
            };
            v_flex()
                .gap_2()
                .border_t_1()
                .border_color(color("border"))
                .p_3()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(format!("#{} {}", key.number, detail.summary.title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(format!(
                            "{} → {}",
                            detail.summary.head_branch, detail.summary.base_branch
                        )),
                )
                .child(div().text_xs().child(format!(
                    "Checks: {:?}   Review: {:?}   Mergeability: {:?}",
                    detail.summary.checks_state,
                    detail.summary.review_decision,
                    detail.summary.mergeability
                )))
                .when(!detail.body.trim().is_empty(), |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(color("textMuted"))
                            .child(detail.body.clone()),
                    )
                })
                .child(
                    h_flex()
                        .gap_1()
                        .child(action("Merge", "merge"))
                        .child(action("Close", "close"))
                        .child(action("Ready", "mark_ready"))
                        .child(action("Reopen", "reopen"))
                        .child(action("Update", "update_branch"))
                        .child(review("Approve", "approve"))
                        .child(review("Request changes", "request_changes"))
                        .child({
                            let diff_project_id = project_id.clone();
                            let diff_key = key.clone();
                            Button::new("pull-request-diff")
                                .label("Files")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |view, _, _, _| {
                                    view.perform(Intent::LoadPullRequestDiff {
                                        project_id: diff_project_id.clone(),
                                        host: Some(diff_key.host.clone()),
                                        repository: diff_key.repository.clone(),
                                        number: diff_key.number,
                                        cursor: None,
                                        commit: None,
                                    });
                                }))
                        }),
                )
                .when_some(diff_next_cursor, |row, cursor| {
                    let owner = owner.clone();
                    let project_id = project_id.clone();
                    let key = key.clone();
                    row.child(
                        Button::new("pull-request-diff-more")
                            .label("More files")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, _| {
                                    view.perform(Intent::LoadPullRequestDiff {
                                        project_id: project_id.clone(),
                                        host: Some(key.host.clone()),
                                        repository: key.repository.clone(),
                                        number: key.number,
                                        cursor: Some(cursor.clone()),
                                        commit: None,
                                    });
                                });
                            }),
                    )
                })
                .when_some(thread_id, |row, thread_id| {
                    let owner = owner.clone();
                    let project_id = project_id.clone();
                    let key = key.clone();
                    let detail_url = detail.summary.url.clone();
                    let linked = linked.clone();
                    let link_button = if let Some(linked) = linked.clone() {
                        let watching = linked.watch.is_some();
                        let owner_for_unlink = owner.clone();
                        let project_for_unlink = project_id.clone();
                        let key_for_unlink = key.clone();
                        let thread_for_unlink = thread_id.clone();
                        let unlink = Button::new("pull-request-unlink")
                            .label("Unlink")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                let _ = owner_for_unlink.update(cx, |view, _| {
                                    view.perform(Intent::UnlinkPullRequest {
                                        thread_id: thread_for_unlink.clone(),
                                        project_id: project_for_unlink.clone(),
                                        host: key_for_unlink.host.clone(),
                                        repository: key_for_unlink.repository.clone(),
                                        number: key_for_unlink.number,
                                    });
                                });
                            });
                        let owner_for_watch = owner.clone();
                        let project_for_watch = project_id.clone();
                        let key_for_watch = key.clone();
                        let thread_for_watch = thread_id.clone();
                        let url_for_watch = linked.url.clone();
                        let watch = Button::new("pull-request-watch")
                            .label(if watching { "Unwatch" } else { "Watch" })
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                let _ = owner_for_watch.update(cx, |view, _| {
                                    view.perform(Intent::SetPullRequestWatch {
                                        thread_id: thread_for_watch.clone(),
                                        project_id: project_for_watch.clone(),
                                        host: key_for_watch.host.clone(),
                                        repository: key_for_watch.repository.clone(),
                                        number: key_for_watch.number,
                                        url: url_for_watch.clone(),
                                        enabled: !watching,
                                    });
                                });
                            });
                        h_flex().gap_1().child(unlink).child(watch)
                    } else {
                        let owner_for_link = owner.clone();
                        let project_for_link = project_id.clone();
                        let key_for_link = key.clone();
                        let thread_for_link = thread_id.clone();
                        h_flex().child(
                            Button::new("pull-request-link")
                                .label("Link to thread")
                                .ghost()
                                .xsmall()
                                .on_click(move |_, _, cx| {
                                    let _ = owner_for_link.update(cx, |view, _| {
                                        view.perform(Intent::LinkPullRequest {
                                            thread_id: thread_for_link.clone(),
                                            project_id: project_for_link.clone(),
                                            host: key_for_link.host.clone(),
                                            repository: key_for_link.repository.clone(),
                                            number: key_for_link.number,
                                            url: detail_url.clone(),
                                        });
                                    });
                                }),
                        )
                    };
                    row.child(link_button)
                })
                .children(detail.comments.iter().take(5).map(|comment| {
                    v_flex()
                        .gap_0p5()
                        .child(
                            div().text_xs().font_weight(FontWeight::MEDIUM).child(
                                comment
                                    .author
                                    .as_ref()
                                    .map(|author| author.login.clone())
                                    .unwrap_or_else(|| "Unknown".to_owned()),
                            ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(color("textMuted"))
                                .child(comment.body.clone()),
                        )
                }))
                .children(diff_files.into_iter().map(|file| {
                    let path = file.path.clone();
                    let viewed = viewed_files.get(&path).copied().unwrap_or(false);
                    let old_path = file.old_path.clone().unwrap_or_else(|| path.clone());
                    let context = file.context.clone();
                    let has_patch = file.patch.is_some();
                    let patch_truncated = file.truncated;
                    let owner = owner.clone();
                    let project_id = project_id.clone();
                    let key_for_file = key.clone();
                    let context_owner = owner.clone();
                    let context_project_id = project_id.clone();
                    let context_key_for_file = key.clone();
                    let context_path = path.clone();
                    let context_old_path = old_path.clone();
                    let context_type = file.change_type;
                    let context_button =
                        Button::new(SharedString::from(format!("diff-context-{}", path)))
                            .label(if context.is_some() {
                                "Reload context"
                            } else {
                                "Context"
                            })
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                let _ = context_owner.update(cx, |view, _| {
                                    view.perform(Intent::LoadPullRequestDiffFileContents {
                                        project_id: context_project_id.clone(),
                                        host: Some(context_key_for_file.host.clone()),
                                        repository: context_key_for_file.repository.clone(),
                                        number: context_key_for_file.number,
                                        commit: None,
                                        change_type: context_type,
                                        old_path: context_old_path.clone(),
                                        new_path: context_path.clone(),
                                    });
                                });
                            });
                    v_flex()
                        .gap_0p5()
                        .pt_1()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(path.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(color("textMuted"))
                                        .child(format!("+{} -{}", file.additions, file.deletions)),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "viewed-file-{}",
                                        path
                                    )))
                                    .label(if viewed { "Viewed" } else { "Mark viewed" })
                                    .ghost()
                                    .xsmall()
                                    .on_click(
                                        move |_, _, cx| {
                                            let _ = owner.update(cx, |view, _| {
                                                view.perform(Intent::SetPullRequestFilesViewed {
                                                    project_id: project_id.clone(),
                                                    host: Some(key_for_file.host.clone()),
                                                    repository: key_for_file.repository.clone(),
                                                    number: key_for_file.number,
                                                    files: vec![PullRequestViewedFileInput {
                                                        path: path.clone(),
                                                        viewed: !viewed,
                                                    }],
                                                });
                                            });
                                        },
                                    ),
                                )
                                .child(context_button),
                        )
                        .when_some(file.patch, |row, patch| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(color("textMuted"))
                                    .child(patch.lines().take(12).collect::<Vec<_>>().join("\n")),
                            )
                        })
                        .when(!has_patch, |row| {
                            row.child(div().text_xs().text_color(color("textMuted")).child(
                                if patch_truncated {
                                    "Textual patch unavailable"
                                } else {
                                    "No textual patch"
                                },
                            ))
                        })
                        .when_some(context, |row, context| {
                            row.child(div().text_xs().text_color(color("textMuted")).child(
                                format!(
                                        "Before: {}\nAfter: {}",
                                        context
                                            .old_contents
                                            .lines()
                                            .take(4)
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                        context
                                            .new_contents
                                            .lines()
                                            .take(4)
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    ),
                            ))
                        })
                }))
                .into_any_element()
        });
        v_flex()
            .id("pull-requests-panel")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .h_8()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(color("border"))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Pull requests"))
                    .child(
                        Button::new("refresh-pull-requests")
                            .icon(icon("refresh-cw"))
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.perform(Intent::LoadPullRequests {
                                    project_id: refresh_project.clone(),
                                    repository: None,
                                    query: None,
                                    include_closed: true,
                                });
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .id("pull-request-list")
                    .gap_1()
                    .p_2()
                    .children(rows),
            )
            .children(detail_view)
            .into_any_element()
    }
}
