//! The Diff tab: the selected scope's patch, its scope menu and view options.
use super::PanelTab;
use crate::{
    app::{
        Desktop,
        ui::{color, icon_button, text_2xs, tint},
    },
    diff::{DiffLayout, DiffView, diff_stat},
};
use agent_core::{
    state::Intent,
    view::checkpoints::{DiffPanelView, DiffRequest, DiffScopeChoice},
};
use gpui_kit::{
    component::{
        Selectable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct DiffState {
    view: Entity<DiffView>,
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
    pub(super) fn new(cx: &mut Context<Desktop>) -> Self {
        Self {
            view: cx.new(|_| DiffView::new("".into(), true)),
            layout: DiffLayout::Stacked,
            wrap: false,
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
        if self.panels.shows(PanelTab::Diff) && !self.panels.diff.loading {
            let key = panel
                .request
                .clone()
                .map(|request| (thread.clone(), request));
            if key.is_some() && key != self.panels.diff.loaded {
                self.panels.diff.loaded = key;
                self.load_diff(&panel);
            }
        }
        let review = self
            .snapshot
            .workspace
            .review
            .clone()
            .filter(|_| !matches!(panel.request, Some(DiffRequest::Branch { .. }) | None));
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
            .filter(|_| !matches!(panel.request, Some(DiffRequest::Branch { .. }) | None));
        let body = if let Some(message) = &panel.empty_message {
            empty_state(message)
        } else if state.loading && review.is_none() {
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
                Some(_) => empty_state("No net changes in this selection."),
                None => empty_state("No patch available for this selection."),
            }
        };
        let base_ref = self
            .snapshot
            .selected_thread
            .as_ref()
            .and_then(|thread| self.snapshot.diff_panels.get(thread))
            .and_then(|selection| match &selection.selection {
                agent_core::view::checkpoints::DiffSelection::Branch { base_ref } => {
                    Some(base_ref.clone())
                }
                _ => None,
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
                    .px_2()
                    .border_b_1()
                    .border_color(tint("border", 0.6))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_3()
                            .child(self.scope_menu(&panel, cx))
                            .when_some(base_ref, |row, base_ref| {
                                row.child(self.base_ref_menu(base_ref, cx))
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .when_some(review.as_ref().filter(|_| file_count > 0), |row, review| {
                                row.child(div().mr_1().child(diff_stat(
                                    Some(review.additions),
                                    Some(review.deletions),
                                )))
                            })
                            .when(panel.request.is_some(), |row| {
                                row.child(
                                    icon_button("refresh-diff", "refresh-cw", "Refresh diff")
                                        .loading(state.loading)
                                        .on_click(cx.listener(move |view, _, _, cx| {
                                            if let Some((_, panel)) = view.diff_panel() {
                                                view.load_diff(&panel);
                                                cx.notify();
                                            }
                                        })),
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
            .when_some(state.error.clone(), |column, error| {
                column.child(
                    div().px_3().py_2().child(
                        text_2xs(div())
                            .text_color(tint("error", 0.8))
                            .child(agent_core::presentation::error::error_message(&error)),
                    ),
                )
            })
            .child(body)
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

    /// The comparison target of "Changes". The Host lists no refs yet, so
    /// only "Automatic" can be chosen.
    fn base_ref_menu(&self, base_ref: Option<String>, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let automatic = base_ref.is_none();
        let label = base_ref.unwrap_or_else(|| "Automatic".into());
        Button::new("diff-base-ref")
            .label(label.clone())
            .accessibility_label(format!("Change comparison target. Currently {label}"))
            .ghost()
            .xsmall()
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let owner = owner.clone();
                menu.item(PopupMenuItem::new("Automatic").checked(automatic).on_click(
                    move |_, _, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.select_diff(Intent::SelectDiffBaseRef { base_ref: None });
                            cx.notify();
                        });
                    },
                ))
                .separator()
                .item(PopupMenuItem::label("No matching refs."))
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
