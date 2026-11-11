//! The desktop's first-run setup in place of the conversations: where to work
//! and its AI, then the projects to bring over from Codex and Claude Code. The
//! import also opens alone from the settings.
use crate::l10n::{localized, tr, tr_format};
use crate::ui::{self, Enabled, Icon, colors};
use crate::{Connection, Navigation, application::Host, connections, environment};
use agent_core::presentation::{
    connections::{AgentAvailability, ConnectionAgent},
    conversation::ActionTone,
    onboarding::{ImportCandidate, OnboardingPage, ProjectImportView, SetupStart},
};
use agent_core::state::{Intent, operations as op};
use nsh_core::prelude::*;

pub(crate) fn page(hosts: Memo<Vec<Host>>) -> View {
    let connection = use_context::<Connection>().unwrap();
    let page = computed(move || connection.state.with(|state| state.onboarding.page()));
    view()
        .flex_1()
        .min_h_0()
        .child(when(
            move || page.get() == Some(OnboardingPage::Environment),
            move || environment_step(connection, hosts),
        ))
        .child(when(
            move || matches!(page.get(), Some(OnboardingPage::Import { .. })),
            move || import_step(connection, page),
        ))
}

/// The setup's card: its step and refresh above the body, its actions below.
fn frame(
    step: impl Fn() -> &'static str + 'static,
    refresh: (
        &'static str,
        impl Fn() + 'static,
        impl Fn() -> bool + Copy + 'static,
    ),
    body: View,
    footer: View,
) -> View {
    let (label, press, enabled) = refresh;
    view().flex_1().min_h_0().child(
        scroll().flex_1().child(
            view().w_full().items_center().px_6().py_8().child(
                view()
                    .w_full()
                    .max_w(px(768.0))
                    .rounded(px(18.0))
                    .border_1()
                    .border_color(colors::border())
                    .bg(colors::RAISED)
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_6()
                            .pt_5()
                            .pb_3()
                            .child(
                                text("bex")
                                    .text_size(px(23.0))
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .child(text(step).flex_1().text_color(colors::muted()))
                            .child(ui::icon_button(Icon::REFRESH, label, press).enabled(enabled)),
                    )
                    .child(body.p_6().gap_5())
                    .child(
                        footer
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .px_6()
                            .py_4()
                            .border_t_1()
                            .border_t_color(colors::border()),
                    ),
            ),
        ),
    )
}

fn connected(connection: Connection) -> bool {
    connection.settings.with(|p| p.connected())
}

/// Step 1: the Host to work on, its AI, and pairing another PC.
fn environment_step(connection: Connection, hosts: Memo<Vec<Host>>) -> View {
    let application = connection.application;
    let adding = signal(false);
    let start = computed(move || {
        connection
            .settings
            .with(|p| p.connection_setup().start(p.connected()))
    });
    let skippable = computed(move || {
        connection
            .settings
            .with(|p| p.connection_setup().can_skip_agents(p.connected()))
    });
    let open_import = move || application.with(|app| app.open_import());
    let refresh = move || {
        if connected(connection) {
            environment::reload_providers(connection);
        } else {
            connection
                .client
                .with(|client| client.reconnect(connection.host.get_untracked()));
        }
    };
    let body = view()
        .child(
            view()
                .flex_row()
                .justify_end()
                .child(crate::settings::language(connection)),
        )
        .child(
            view()
                .gap_2()
                .child(
                    text(tr("どこで作業しますか？"))
                        .text_size(px(24.0))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    text(tr(
                        "手元のPCでも、クラウドでも。いつもの環境でAIを使えます。",
                    ))
                    .text_color(colors::muted()),
                ),
        )
        .child(each(
            move || hosts.get(),
            |host: &Host| host.id.clone(),
            move |host, _| choice(connection, host.get_untracked()),
        ))
        .child(ui::text_button(
            tr("クラウドVM・別のPCに接続"),
            move || adding.update(|open| *open = !*open),
        ))
        .child(when(move || adding.get(), connections::pairing))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .p_4()
                .rounded(px(12.0))
                .border_1()
                .border_color(colors::border())
                .text_color(colors::muted())
                .child(ui::icon(Icon::NETWORK, 16.0))
                .child(text(tr("Bexの実行環境")).flex_1())
                .child(text(tr("今後対応")).text_size(px(12.0))),
        );
    let footer = view()
        .child(
            text(tr("次に、既存のプロジェクトを選んで取り込めます。"))
                .flex_1()
                .min_w_0()
                .text_size(px(12.0))
                .text_color(colors::muted()),
        )
        .child(when(
            move || skippable.get(),
            move || ui::text_button(tr("AIは後で設定"), open_import),
        ))
        .child(
            ui::action_button(
                move || {
                    SharedString::from(match start.get() {
                        SetupStart::Connecting => tr("接続中…"),
                        SetupStart::Ready => tr("プロジェクトを選ぶ"),
                        SetupStart::Checking => tr("AIを確認中…"),
                        SetupStart::LoginRequired => tr("AIにログインしてください"),
                    })
                },
                ActionTone::Primary,
                open_import,
            )
            .enabled(move || start.get() == SetupStart::Ready),
        );
    frame(
        || tr("1 / 2 環境とAI"),
        (tr("接続とAIの状態を再確認"), refresh, || true),
        body,
        footer,
    )
}

/// A Host to work on; the chosen one shows its AI.
fn choice(connection: Connection, host: Host) -> View {
    let id = host.id.clone();
    let chosen = computed({
        let id = host.id.clone();
        move || connection.host.get() == id
    });
    let label = {
        let label = stored(host.label());
        move || label.get()
    };
    view()
        .gap_3()
        .p_4()
        .rounded(px(12.0))
        .border_1()
        .border_color(colors::border())
        .style_when(move || chosen.get(), |s| s.border_color(colors::ACCENT))
        .bg(colors::SURFACE)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(ui::labeled_button(
                    if crate::client::saved(&host.id) {
                        Icon::NETWORK
                    } else {
                        Icon::MONITOR
                    },
                    label,
                    label,
                    move || {
                        connection
                            .application
                            .with(|app| app.select_host(id.clone()))
                    },
                ))
                .child(view().flex_1())
                .child(when(
                    move || chosen.get(),
                    move || {
                        text(move || environment::connection_status(connection))
                            .text_size(px(12.0))
                            .text_color(colors::muted())
                    },
                )),
        )
        .child(when(move || chosen.get(), move || agents(connection)))
}

/// The chosen Host's AI: each provider's state and what it needs next.
fn agents(connection: Connection) -> View {
    view()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_t_color(colors::border())
        .child(
            text(tr("この環境で使えるAI"))
                .text_size(px(12.0))
                .text_color(colors::muted()),
        )
        .child(each(
            move || environment::agents(connection),
            |agent: &ConnectionAgent| agent.provider,
            move |agent, _| {
                let provider = agent.with_untracked(|a| a.provider);
                let needs_login = move || {
                    agent.with(|a| {
                        a.enabled
                            && matches!(
                                a.availability,
                                AgentAvailability::LoginRequired | AgentAvailability::Unavailable
                            )
                    })
                };
                view()
                    .gap_1()
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(text(move || agent.with(|a| a.name.clone())).flex_1())
                            .child(
                                text(move || agent.with(|a| a.label.clone()))
                                    .text_size(px(13.0))
                                    .text_color(colors::muted()),
                            )
                            .child(when(needs_login, move || {
                                ui::text_button(tr("ログイン"), move || {
                                    connection.application.with(|app| {
                                        app.navigate(Navigation::Settings(true));
                                        app.navigate(Navigation::Provider(Some(provider)));
                                    })
                                })
                                .enabled(move || connected(connection))
                            }))
                            .child(environment::provider_switch(connection, agent)),
                    )
                    .child(
                        text(move || agent.with(environment::agent_error))
                            .text_size(px(12.0))
                            .text_color(colors::muted())
                            .selectable(true),
                    )
            },
        ))
}

/// Step 2 of a first run, or alone after it: the projects the Host's Codex and
/// Claude Code history names.
fn import_step(connection: Connection, page: Memo<Option<OnboardingPage>>) -> View {
    let application = connection.application;
    let first_run = move || page.get() == Some(OnboardingPage::Import { first_run: true });
    let view_state = computed(move || {
        connection
            .publication
            .with(|p| p.snapshot.project_import_view())
    });
    let busy = move || view_state.with(ProjectImportView::busy);
    let dispatch = move |intent| connection.dispatch(intent);
    let scanned = move || view_state.with(|v| v.candidates.is_some());
    let body = view()
        .child(
            view()
                .gap_2()
                .child(
                    text(tr("これまでのプロジェクトを引き継ぐ"))
                        .text_size(px(16.0))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    text(tr(
                        "CodexとClaude Codeの履歴から選べます。元の会話をそのまま再開できます。",
                    ))
                    .text_size(px(13.0))
                    .text_color(colors::muted()),
                )
                .child(
                    text(tr(
                        "最近30日以内に使い、会話が3件以上あるGitリポジトリを初期選択しています。",
                    ))
                    .text_size(px(13.0))
                    .text_color(colors::muted()),
                ),
        )
        .child(when(
            move || view_state.with(|v| v.scanning),
            || text(tr("既存のプロジェクトを探しています…")),
        ))
        .child(when(scanned, move || {
            view()
                .flex_row()
                .gap_2()
                .child(
                    ui::text_button(tr("すべて選択"), move || {
                        dispatch(Intent::SelectAllImportProjects { selected: true })
                    })
                    .enabled(move || !busy()),
                )
                .child(
                    ui::text_button(tr("選択を解除"), move || {
                        dispatch(Intent::SelectAllImportProjects { selected: false })
                    })
                    .enabled(move || !busy()),
                )
        }))
        .child(when(
            move || view_state.with(|v| v.candidates.as_ref().is_some_and(Vec::is_empty)),
            || {
                text(tr(
                    "取り込めるプロジェクトは見つかりませんでした。後から設定でも取り込めます。",
                ))
            },
        ))
        .child(view().gap_2().child(each(
            move || view_state.with(|v| v.candidates.clone().unwrap_or_default()),
            |candidate: &ImportCandidate| candidate.path.clone(),
            move |candidate, _| project(connection, candidate),
        )))
        .child(text(move || {
            view_state.with(|v| {
                v.warnings
                    .iter()
                    .map(|(provider, warning)| {
                        format!("{provider}: {}", localized(warning.clone()))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }))
        .child(
            text(move || {
                view_state.with(|v| {
                    (v.imported > 0)
                        .then(|| tr_format("{0}件のプロジェクトを取り込みました。", [&v.imported]))
                        .into_iter()
                        .chain(
                            v.errors.iter().map(|(path, error)| {
                                format!("{path}: {}", localized(error.clone()))
                            }),
                        )
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            })
            .selectable(true),
        );
    let importing = move || view_state.with(|v| v.importing);
    let footer = view()
        .child(
            ui::text_button(tr("戻る"), move || {
                application.with(|app| app.close_import())
            })
            .enabled(move || !importing()),
        )
        .child(view().flex_1())
        .child(
            ui::text_button(
                move || {
                    SharedString::from(if first_run() {
                        tr("今はスキップ")
                    } else {
                        tr("閉じる")
                    })
                },
                move || application.with(|app| app.finish_onboarding()),
            )
            .enabled(move || !importing()),
        )
        .child(
            ui::action_button(
                move || {
                    SharedString::from(view_state.with(|v| {
                        if v.busy() {
                            tr("読み込み中…").to_owned()
                        } else {
                            tr_format("{0}件を取り込む", [&v.selected])
                        }
                    }))
                },
                ActionTone::Primary,
                move || application.with(|app| app.import_projects()),
            )
            .enabled(move || view_state.with(|v| v.can_import)),
        );
    frame(
        move || {
            if first_run() {
                tr("2 / 2 プロジェクトの取り込み")
            } else {
                tr("プロジェクトの取り込み")
            }
        },
        (
            tr("既存のプロジェクトを再検索"),
            move || dispatch(Intent::ScanProjects(op::ScanProjects {})),
            move || !busy(),
        ),
        body,
        footer,
    )
}

/// A project to import: chosen with its switch, or already imported.
fn project(connection: Connection, candidate: ReadSignal<ImportCandidate>) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(colors::border())
        .child(
            ui::switch(
                move || candidate.with(|c| c.name.clone()),
                move || candidate.with(|c| c.checked),
                move |selected| {
                    connection.dispatch(Intent::SelectImportProject {
                        path: candidate.with_untracked(|c| c.path.clone()),
                        selected,
                    })
                },
            )
            .enabled(move || candidate.with(|c| c.selectable)),
        )
        .child(
            view()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(text(move || candidate.with(|c| c.name.clone())))
                .child(
                    text(move || candidate.with(|c| c.path.clone()))
                        .text_size(px(12.0))
                        .text_color(colors::muted()),
                ),
        )
        .child(
            text(move || {
                candidate.with(|c| {
                    if c.registered {
                        tr("取り込み済み").to_owned()
                    } else {
                        tr_format("{0}件の会話 · {1}", [&c.conversations, &c.providers])
                    }
                })
            })
            .text_size(px(12.0))
            .text_color(colors::muted()),
        )
}
