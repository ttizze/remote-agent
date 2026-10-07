//! The Diff tab: the selected scope's patch, its scope menu and view options.
use super::PanelTab;
use crate::{
    app::{
        Desktop,
        ui::{color, icon, icon_button, text_2xs, tint},
    },
    diff::{DiffLayout, DiffView, DiffViewEvent, diff_stat},
};
use agent_core::{
    state::Intent,
    view::checkpoints::{BaseRefChoice, DiffPanelView, DiffRequest, DiffScopeChoice, GitDiffView},
};
use gpui_kit::{
    component::{
        Selectable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::{DropdownMenu, PopupMenuItem},
        popover::Popover,
        switch::Switch,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct DiffState {
    view: Entity<DiffView>,
    /// The base picker is open, and its search field.
    base_open: bool,
    base_query: Entity<InputState>,
    layout: DiffLayout,
    wrap: bool,
    /// The thread and request whose patch was last asked for.
    loaded: Option<(String, DiffRequest)>,
    loading: bool,
    error: Option<String>,
    /// The thread and reveal request already scrolled to.
    revealed: Option<(String, u64)>,
}
impl DiffState {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Desktop>,
        subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        let base_query = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        subscriptions.push(cx.subscribe(&base_query, |view, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().trim().to_owned();
                view.perform(Intent::SearchDiffBaseRefs { query });
                cx.notify();
            }
        }));
        let view = cx.new(|_| DiffView::new("".into(), true));
        subscriptions.push(cx.subscribe(&view, |desktop, _, event, cx| {
            desktop.perform(match event {
                DiffViewEvent::LoadMore => Intent::LoadMoreDiffFiles,
                DiffViewEvent::Retry(path) => Intent::RevealDiffFile {
                    path: path.clone(),
                    retry: true,
                },
            });
            cx.notify();
        }));
        Self {
            view,
            base_open: false,
            base_query,
            wrap: crate::app::ui_word_wrap(),
            layout: DiffLayout::Stacked,
            loaded: None,
            loading: false,
            error: None,
            revealed: None,
        }
    }
    pub(super) fn reset(&mut self) {
        self.loaded = None;
        self.loading = false;
        self.error = None;
        self.revealed = None;
    }
}

impl Desktop {
    fn diff_panel(&self) -> Option<(String, DiffPanelView)> {
        let thread = self.thread_id()?;
        Some((thread.clone(), self.snapshot.diff(thread)))
    }

    /// Loads the selected patch when the Diff tab shows a selection it has not
    /// asked for yet, and keeps the view on the received patch.
    pub(super) fn sync_diff(&mut self, cx: &mut Context<Self>) {
        let Some((thread, panel)) = self.diff_panel() else {
            return;
        };
        if self.panel_shows(PanelTab::Diff) && !self.panels.diff.loading {
            let key = panel
                .request
                .clone()
                .map(|request| (thread.clone(), request));
            if key.is_some() && key != self.panels.diff.loaded {
                self.panels.diff.loaded = key;
                self.load_diff(&panel);
            }
        }
        // A diff too large to send whole shows file by file.
        let files = panel
            .git
            .as_ref()
            .and_then(|git| git.files_revision.clone())
            .and_then(|key| Some((key, self.snapshot.review_files(thread.clone())?)));
        if let Some((key, files)) = files {
            let (layout, wrap) = (self.panels.diff.layout, self.panels.diff.wrap);
            self.panels.diff.view.update(cx, |view, cx| {
                view.set_files(&key, &files, cx);
                view.set_layout(layout, wrap, cx);
            });
            return;
        }
        let review = self
            .snapshot
            .workspace
            .review
            .clone()
            .filter(|_| panel.request.is_some());
        let source = review.as_ref().map_or("", |review| review.diff.as_str());
        let stats = review
            .iter()
            .flat_map(|review| &review.files)
            .map(|file| (file.path.clone(), (file.additions, file.deletions)))
            .collect();
        let (layout, wrap) = (self.panels.diff.layout, self.panels.diff.wrap);
        let reveal = (review.is_some()
            && !self.panels.diff.loading
            && panel.reveal_request_id > 0
            && self.panels.diff.revealed != Some((thread.clone(), panel.reveal_request_id)))
        .then_some(panel.selected_file_path)
        .flatten();
        if reveal.is_some() {
            self.panels.diff.revealed = Some((thread, panel.reveal_request_id));
        }
        self.panels.diff.view.update(cx, |view, cx| {
            view.set_stats(stats);
            view.set_source(source, cx);
            view.set_layout(layout, wrap, cx);
            if let Some(path) = reveal {
                view.reveal(&path, cx);
            }
        });
    }

    /// Asks the Host for the panel's patch.
    fn load_diff(&mut self, panel: &DiffPanelView) {
        if panel.request.is_none() {
            return;
        }
        let intent = Intent::LoadDiff;
        self.panels.diff.loading = true;
        self.panels.diff.error = None;
        self.perform_then(intent, |view, result, _, cx| {
            view.panels.diff.loading = false;
            view.panels.diff.error = result.as_ref().err().cloned();
            cx.notify();
        });
    }

    /// Runs an intent that changes the selection and loads its patch.
    fn select_diff(&mut self, intent: Intent) {
        self.panels.diff.loading = true;
        self.panels.diff.error = None;
        self.perform_then(intent, |view, result, _, cx| {
            view.panels.diff.loading = false;
            view.panels.diff.error = result.as_ref().err().cloned();
            view.panels.diff.loaded = view
                .diff_panel()
                .and_then(|(thread, panel)| panel.request.map(|request| (thread, request)));
            cx.notify();
        });
    }

    /// Wraps diff lines, as the word wrap preference just changed to.
    pub(crate) fn set_diff_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.panels.diff.wrap = wrap;
        self.sync_diff(cx);
    }

    /// Shows a turn (the latest when `None`) and scrolls to `file_path`.
    pub(super) fn select_diff_turn(&mut self, run_id: Option<String>, file_path: Option<String>) {
        let run_id = run_id.or_else(|| {
            self.diff_panel()
                .and_then(|(_, panel)| panel.turns.first().map(|turn| turn.run_id.clone()))
        });
        match run_id {
            Some(run_id) => self.select_diff(Intent::SelectDiffTurn { run_id, file_path }),
            None => self.select_diff(Intent::SelectDiffScope {
                choice: DiffScopeChoice::LatestTurn,
            }),
        }
    }

    pub(super) fn render_diff(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((_, panel)) = self.diff_panel() else {
            return empty_state("Select a thread to inspect turn diffs.");
        };
        let state = &self.panels.diff;
        let review = self
            .snapshot
            .workspace
            .review
            .clone()
            .filter(|_| panel.request.is_some());
        let checkout = !matches!(panel.request, Some(DiffRequest::Turn { .. }) | None);
        let git = panel.git.clone().filter(|_| checkout);
        let is_repo = panel.git.as_ref().is_none_or(|git| git.is_repo);
        let loading = match &git {
            Some(git) => git.loading,
            None => state.loading,
        };
        // Totals of a diff shown file by file count every file of its list.
        let file_totals = state
            .view
            .read(cx)
            .file_totals()
            .filter(|_| git.as_ref().is_some_and(|git| git.files_revision.is_some()));
        let reading_files = file_totals.is_some_and(|(_, _, pending)| pending);
        let error = match &git {
            Some(git) => git.error.clone(),
            None => state.error.clone(),
        };
        let body = if !is_repo {
            empty_state("Turn diffs are unavailable because this project is not a git repository.")
        } else if let Some(message) = &panel.empty_message {
            empty_state(message)
        } else {
            let content = if file_totals.is_some() {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(state.view.clone())
                    .into_any_element()
            } else if loading && (review.is_none() || checkout) {
                empty_state(match panel.request {
                    Some(DiffRequest::Turn { .. }) => "Loading checkpoint diff...",
                    Some(DiffRequest::Unstaged { .. }) => "Loading uncommitted changes...",
                    _ => "Loading changes...",
                })
            } else {
                match &review {
                    Some(review) if !review.diff.trim().is_empty() => div()
                        .flex_1()
                        .min_h_0()
                        .child(state.view.clone())
                        .into_any_element(),
                    Some(review) if review.diff.is_empty() && error.is_none() => {
                        empty_state("No net changes in this selection.")
                    }
                    _ if error.is_some() => div().flex_1().into_any_element(),
                    _ => empty_state("No patch available for this selection."),
                }
            };
            let has_patch = file_totals.is_some()
                || review
                    .as_ref()
                    .is_some_and(|review| !review.diff.trim().is_empty());
            v_flex()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .bg(color("canvas"))
                .when(git.as_ref().is_some_and(|git| git.truncated), |column| {
                    column.child(
                        text_2xs(div())
                            .flex_shrink_0()
                            .border_b_1()
                            .border_color(tint("border", 0.7))
                            .bg(tint("muted", 0.4))
                            .px_3()
                            .py(px(6.))
                            .text_color(color("textMuted"))
                            .child("This preview exceeds the size limit. Changes shown are incomplete."),
                    )
                })
                .when_some(error.filter(|_| !has_patch), |column, error| {
                    column.child(
                        div().px_3().child(
                            text_2xs(div())
                                .mb_2()
                                .text_color(tint("error", 0.8))
                                .child(agent_core::presentation::error::error_message(&error)),
                        ),
                    )
                })
                .child(content)
                .into_any_element()
        };
        let comparison = git
            .as_ref()
            .filter(|_| matches!(panel.request, Some(DiffRequest::Branch { .. })))
            .and_then(|git| {
                let base = git.base_ref.clone()?;
                Some((git.clone(), base))
            });
        let file_count = state.view.read(cx).file_count();
        let all_folded = state.view.read(cx).all_folded();
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .h_10()
                    .flex_shrink_0()
                    .justify_between()
                    .gap_2()
                    .px_4()
                    .border_b_1()
                    .border_color(tint("border", 0.6))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_3()
                            .child(self.scope_menu(&panel, cx))
                            .when_some(comparison, |row, (git, base)| {
                                row.child(self.comparison(&git, base, cx))
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .when_some(
                                file_totals
                                    .map(|(additions, deletions, _)| (additions, deletions))
                                    .or_else(|| {
                                        review
                                            .as_ref()
                                            .map(|review| (review.additions, review.deletions))
                                    })
                                    .filter(|_| file_count > 0),
                                |row, (additions, deletions)| {
                                    row.child(
                                        div()
                                            .mr_1()
                                            .child(diff_stat(Some(additions), Some(deletions))),
                                    )
                                },
                            )
                            .when(checkout && is_repo, |row| {
                                row.child(
                                    icon_button(
                                        "refresh-diff",
                                        "refresh-cw",
                                        if loading || reading_files {
                                            "Refreshing diff\u{2026}"
                                        } else {
                                            "Refresh diff"
                                        },
                                    )
                                    .loading(loading || reading_files)
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            if let Some((_, panel)) = view.diff_panel() {
                                                view.load_diff(&panel);
                                                cx.notify();
                                            }
                                        },
                                    )),
                                )
                            })
                            .when(file_count > 0, |row| {
                                let label = if all_folded {
                                    "Expand all files"
                                } else {
                                    "Collapse all files"
                                };
                                row.child(
                                    icon_button(
                                        "collapse-diff-files",
                                        if all_folded {
                                            "chevrons-up-down"
                                        } else {
                                            "chevrons-down-up"
                                        },
                                        label,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, _, cx| {
                                            view.panels
                                                .diff
                                                .view
                                                .update(cx, |view, cx| view.toggle_all_files(cx));
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .child(self.layout_toggle(cx))
                            .child(
                                icon_button(
                                    "diff-wrap",
                                    "text-wrap",
                                    if state.wrap {
                                        "Disable line wrapping"
                                    } else {
                                        "Enable line wrapping"
                                    },
                                )
                                .selected(state.wrap)
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        view.panels.diff.wrap = !view.panels.diff.wrap;
                                        view.sync_diff(cx);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                icon_button(
                                    "diff-whitespace",
                                    "pilcrow",
                                    &panel.whitespace_toggle_label,
                                )
                                .selected(panel.ignore_whitespace)
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.select_diff(Intent::SetDiffIgnoreWhitespace {
                                            ignore: !panel.ignore_whitespace,
                                        });
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    /// `head → base`: the head ref, then the base picker.
    fn comparison(&self, git: &GitDiffView, base: String, cx: &mut Context<Self>) -> AnyElement {
        let head = git.head_ref.clone().unwrap_or_else(|| "HEAD".into());
        let tooltip: SharedString = git
            .comparison_label
            .clone()
            .unwrap_or_else(|| format!("{head} \u{2192} {base}"))
            .into();
        h_flex()
            .min_w_0()
            .max_w_full()
            .gap_2()
            .overflow_hidden()
            .text_xs()
            .text_color(color("textMuted"))
            .child(
                h_flex()
                    .id("diff-head")
                    .min_w_0()
                    .gap_2()
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .child(div().max_w(px(192.)).truncate().child(head))
                    .child(
                        icon("arrow-right")
                            .size(px(14.))
                            .flex_shrink_0()
                            .opacity(0.7),
                    ),
            )
            .child(self.base_ref_picker(git, base, cx))
            .into_any_element()
    }

    fn set_base_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.panels.diff.base_open = open;
        if open {
            self.perform(Intent::SearchDiffBaseRefs {
                query: String::new(),
            });
        } else {
            self.panels
                .diff
                .base_query
                .update(cx, |query, cx| query.set_value("", window, cx));
        }
        cx.notify();
    }

    /// The comparison target of "Changes": a searchable list of local and
    /// remote branches after "Automatic".
    fn base_ref_picker(
        &self,
        git: &GitDiffView,
        base: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = cx.entity().downgrade();
        let query = self.panels.diff.base_query.clone();
        let focus = query.read(cx).focus_handle(cx);
        let choices = git.base_ref_choices.clone();
        let trigger = Button::new("diff-base-ref")
            .ghost()
            .xsmall()
            .max_w(px(192.))
            .text_color(color("textMuted"))
            .accessibility_label(format!("Change comparison target. Currently {base}"))
            .child(div().min_w_0().truncate().child(base))
            .dropdown_caret(true);
        let on_open = owner.clone();
        Popover::new("diff-base-ref-popover")
            .anchor(Anchor::TopLeft)
            .open(self.panels.diff.base_open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = on_open.update(cx, |view, cx| view.set_base_picker(open, window, cx));
            })
            .track_focus(&focus)
            .p_0()
            .trigger(trigger)
            .content(move |_, _, _| base_ref_list(&choices, &query, &owner).into_any_element())
            .into_any_element()
    }

    /// The "Diff scope: <label>" menu: Changes, Uncommitted, Latest turn and
    /// a "Turn" submenu, newest first.
    fn scope_menu(&self, panel: &DiffPanelView, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let scopes = panel.scopes.clone();
        let turns = panel.turns.clone();
        let format = self.snapshot.preferences.timestamp_format;
        Button::new("diff-scope")
            .label(panel.scope_label.clone())
            .accessibility_label(format!("Diff scope: {}", panel.scope_label))
            .xsmall()
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, window, cx| {
                for scope in &scopes {
                    let owner = owner.clone();
                    let choice = scope.choice.clone();
                    menu = menu.item(
                        PopupMenuItem::new(scope.label.clone())
                            .checked(scope.selected)
                            .on_click(move |_, _, cx| {
                                let choice = choice.clone();
                                let _ = owner.update(cx, |view, cx| {
                                    view.select_diff(Intent::SelectDiffScope { choice });
                                    cx.notify();
                                });
                            }),
                    );
                }
                let owner = owner.clone();
                let turns = turns.clone();
                menu.submenu("Turn", window, cx, move |mut menu, _, _| {
                    for turn in &turns {
                        let owner = owner.clone();
                        let run_id = turn.run_id.clone();
                        let time = turn
                            .completed_at_ms
                            .and_then(chrono::DateTime::from_timestamp_millis)
                            .map(|at| {
                                agent_core::view::time::time_of_day(
                                    &at.with_timezone(&chrono::Local),
                                    format,
                                )
                            })
                            .unwrap_or_default();
                        let label = turn.label.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, _| {
                                h_flex().w_full().gap_2().child(label.clone()).child(
                                    div()
                                        .ml_auto()
                                        .text_xs()
                                        .text_color(color("textMuted"))
                                        .child(time.clone()),
                                )
                            })
                            .checked(turn.selected)
                            .on_click(move |_, _, cx| {
                                let run_id = run_id.clone();
                                let _ = owner.update(cx, |view, cx| {
                                    view.select_diff(Intent::SelectDiffScope {
                                        choice: DiffScopeChoice::Turn { run_id },
                                    });
                                    cx.notify();
                                });
                            }),
                        );
                    }
                    menu
                })
            })
    }

    /// Stacked or split, as a two-segment toggle.
    fn layout_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.panels.diff.layout;
        let segment = |id: &'static str, icon_name: &'static str, label: &'static str, value| {
            icon_button(id, icon_name, label)
                .selected(layout == value)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.panels.diff.layout = value;
                    view.sync_diff(cx);
                    cx.notify();
                }))
        };
        h_flex()
            .rounded_md()
            .border_1()
            .border_color(color("border"))
            .child(segment(
                "diff-stacked",
                "rows-3",
                "Stacked diff view",
                DiffLayout::Stacked,
            ))
            .child(segment(
                "diff-split",
                "columns-2",
                "Split diff view",
                DiffLayout::Split,
            ))
    }
}

/// The base picker's popup: the search field, the "Branch / Remote" header
/// and the choices.
fn base_ref_list(
    choices: &[BaseRefChoice],
    query: &Entity<InputState>,
    owner: &WeakEntity<Desktop>,
) -> Div {
    let header = h_flex()
        .pl_3()
        .pr(px(26.))
        .pt_2()
        .pb(px(6.))
        .border_b_1()
        .border_color(tint("border", 0.7))
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color("textMuted"))
        .child(div().flex_1().child("BRANCH"))
        .child(div().w(px(32.)).text_right().child("REMOTE"));
    let rows = choices.iter().map(|choice| {
        let select = owner.clone();
        let value = choice.value.clone();
        let trailing = match (&choice.local, &choice.remote) {
            (Some(local), Some(remote)) => {
                let (switch, local, remote) = (owner.clone(), local.clone(), remote.clone());
                Some(
                    Switch::new(SharedString::from(format!("base-remote-{}", choice.id)))
                        .small()
                        .checked(choice.value == remote)
                        .accessibility_label(format!("Use remote version of {}", choice.label))
                        .on_click(move |on: &bool, _, cx| {
                            cx.stop_propagation();
                            let base_ref = if *on { remote.clone() } else { local.clone() };
                            let _ = switch.update(cx, |view, cx| {
                                view.select_diff(Intent::SelectDiffBaseRef {
                                    base_ref: Some(base_ref),
                                });
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                )
            }
            (None, Some(_)) => Some(
                div()
                    .id(SharedString::from(format!(
                        "base-remote-only-{}",
                        choice.id
                    )))
                    .tooltip(|window, cx| Tooltip::new("Remote only").build(window, cx))
                    .child(icon("check").size(px(12.)).text_color(color("textMuted")))
                    .into_any_element(),
            ),
            _ => None,
        };
        h_flex()
            .id(SharedString::from(format!("base-ref-{}", choice.id)))
            .w_full()
            .min_h_7()
            .px_2()
            .py_1()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .when(choice.selected, |row| row.bg(color("text").opacity(0.08)))
            .hover(|row| row.bg(color("accentSurface")))
            .on_click(move |_, window, cx| {
                let base_ref = (!value.is_empty()).then(|| value.clone());
                let _ = select.update(cx, |view, cx| {
                    view.select_diff(Intent::SelectDiffBaseRef { base_ref });
                    view.set_base_picker(false, window, cx);
                });
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr_2()
                    .truncate()
                    .child(choice.label.clone()),
            )
            .child(h_flex().w(px(32.)).justify_end().children(trailing))
    });
    let list = if choices.is_empty() {
        div()
            .p_2()
            .text_center()
            .text_sm()
            .text_color(color("textMuted"))
            .child("No matching refs.")
            .into_any_element()
    } else {
        v_flex()
            .id("base-ref-list")
            .max_h(px(256.))
            .overflow_y_scroll()
            .p_1()
            .children(rows)
            .into_any_element()
    };
    v_flex()
        .w(px(288.))
        .max_h(px(368.))
        .overflow_hidden()
        .child(
            div().px_3().pt(px(10.)).child(
                div()
                    .pb(px(6.))
                    .border_b_1()
                    .border_color(tint("border", 0.7))
                    .child(
                        Input::new(query).appearance(false).small().prefix(
                            icon("search")
                                .size(px(16.))
                                .text_color(color("textMuted").opacity(0.55)),
                        ),
                    ),
            ),
        )
        .child(header)
        .child(list)
}

fn empty_state(message: &str) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .px_5()
        .text_center()
        .text_xs()
        .text_color(tint("textMuted", 0.7))
        .child(message.to_owned())
        .into_any_element()
}
