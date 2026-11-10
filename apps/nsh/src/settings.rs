//! The desktop settings, as T3 Code's: a blurred overlay whose searchable
//! sections sit beside a breadcrumb, each section a set of grouped rows.
use crate::ui::{self, Enabled, Icon, colors};
use crate::{
    Connection, Navigation,
    application::Host,
    environment,
    l10n::{self, localized, tr, tr_format},
};
use agent_core::{
    localization::UiLanguage,
    presentation::connections::ConnectionAgent,
    state::{ComposerPreferences, Intent, SendShortcut},
    updates::{Channel, UpdatePhase},
};
use agent_protocol::{operations::FollowUp, session::ProviderKind};
use nsh_core::{
    element::{ImageSrc, overlay as overlay_element},
    prelude::*,
};
use serde_json::json;

mod worktrees;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Page {
    General,
    Providers,
    Environments,
    Worktrees,
}

impl Page {
    const ALL: [Self; 4] = [
        Self::General,
        Self::Providers,
        Self::Environments,
        Self::Worktrees,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => tr("一般"),
            Self::Providers => tr("プロバイダー"),
            Self::Environments => tr("環境"),
            Self::Worktrees => tr("ワークツリー"),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::General => tr("この端末で使用するアプリの設定です。"),
            Self::Providers => tr("選択した環境のプロバイダー・アカウント・更新を管理します。"),
            Self::Environments => tr("作業する環境と、このPCへ接続する端末を管理します。"),
            Self::Worktrees => tr("新しい会話の作業場所と、作成済みのワークツリーを管理します。"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::General => Icon::SLIDERS,
            Self::Providers => Icon::BOT,
            Self::Environments => Icon::SERVER,
            Self::Worktrees => Icon::FOLDER_TREE,
        }
    }
}

pub(crate) fn overlay(hosts: Memo<Vec<Host>>) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let page = signal(Page::General);
    // A provider chosen from outside (the model picker's accounts) shows its page.
    let previous_provider = stored(None::<ProviderKind>);
    effect(move || {
        let provider = connection.state.with(|state| state.provider).or_else(|| {
            connection
                .settings
                .with(|settings| settings.account_login().map(|login| login.provider))
        });
        if previous_provider.replace(provider) != provider && provider.is_some() {
            page.set(Page::Providers);
        }
    });
    let close = move || {
        connection
            .application
            .with(|app| app.navigate(Navigation::Settings(false)));
    };
    environment::read_settings(connection);
    overlay_element()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .items_center()
        .justify_center()
        .p(px(40.0))
        // The backdrop is a separate standard button.  Making the overlay
        // itself pressable turns the entire settings tree into one AppKit
        // accessibility button and also makes it its own press target.
        .on_cancel(close)
        .child(
            view()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .bottom(px(0.0))
                .bg(rgba(0x080808a3))
                .backdrop_blur(px(4.0))
                .role(AccessibilityRole::Button)
                .label(SharedString::from(tr("設定を閉じる")))
                .on_press(close),
        )
        .child(
            view()
                .flex_row()
                .w_full()
                .h_full()
                .max_w(px(1280.0))
                .max_h(px(960.0))
                .rounded(px(16.0))
                .border_1()
                .border_color(colors::BORDER)
                .overflow_hidden()
                .bg(colors::BACKGROUND)
                .child(navigation(page))
                .child(
                    view().flex_1().min_w_0().child(header(page, close)).child(
                        scroll().flex_1().child(
                            view()
                                .w_full()
                                .max_w(px(928.0))
                                .p_8()
                                .gap_8()
                                .child(heading(page))
                                .child(when(
                                    move || page.get() == Page::General,
                                    move || general(connection),
                                ))
                                .child(when(
                                    move || page.get() == Page::Providers,
                                    move || providers(connection),
                                ))
                                .child(when(
                                    move || page.get() == Page::Environments,
                                    move || environments(connection, hosts),
                                ))
                                .child(when(
                                    move || page.get() == Page::Worktrees,
                                    move || worktrees::page(connection),
                                )),
                        ),
                    ),
                ),
        )
}

/// The sections, filtered by the search above them.
fn navigation(page: Signal<Page>) -> View {
    let query = signal(String::new());
    let visible = move || {
        let query = query.get().trim().to_lowercase();
        Page::ALL
            .into_iter()
            .filter(|page| query.is_empty() || page.title().to_lowercase().contains(&query))
            .collect::<Vec<_>>()
    };
    view()
        .w(px(240.0))
        .flex_shrink_0()
        .h_full()
        .p_3()
        .gap_1()
        .bg(colors::SIDEBAR)
        .border_r_1()
        .border_r_color(colors::BORDER)
        .child(ui::search(query, tr("設定を検索")).h(px(32.0)).mb_2())
        .child(
            text(tr("設定"))
                .text_size(px(12.0))
                .text_color(colors::MUTED)
                .px_2()
                .py_1(),
        )
        .child(each(
            visible,
            |page: &Page| *page,
            move |item, _| {
                let item = item.get_untracked();
                nsh_base::button()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(32.0))
                    .px_2()
                    .rounded(px(ui::RADIUS))
                    .text_color(colors::FOREGROUND)
                    .hover(|s| s.bg(colors::ROW_HOVER))
                    .style_when(move || page.get() == item, |s| s.bg(colors::SELECTED))
                    .on_press(move || page.set(item))
                    .child(ui::icon(item.icon(), 16.0))
                    .child(text(item.title()).text_size(px(14.0)))
            },
        ))
}

/// The breadcrumb and the close button.
fn header(page: Signal<Page>, close: impl Fn() + 'static) -> View {
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .h(px(48.0))
        .flex_shrink_0()
        .px_5()
        .border_b_1()
        .border_b_color(colors::BORDER)
        .child(
            text(tr("設定"))
                .text_size(px(14.0))
                .text_color(colors::MUTED),
        )
        .child(text("/").text_size(px(14.0)).text_color(colors::MUTED))
        .child(
            text(move || page.get().title())
                .flex_1()
                .text_size(px(14.0)),
        )
        .child(ui::icon_button(Icon::CLOSE, tr("閉じる"), close))
}

fn heading(page: Signal<Page>) -> View {
    view()
        .gap_2()
        .child(
            text(move || page.get().title())
                .text_size(px(26.0))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            text(move || page.get().description())
                .text_size(px(14.0))
                .text_color(colors::MUTED),
        )
}

fn general(connection: Connection) -> View {
    let connected = move || connection.settings.with(|p| p.connected());
    ui::card(vec![
        ui::setting(
            "Bex Host",
            move || environment::host_software(connection),
            ui::text_button(tr("バージョン情報を再読み込み"), move || {
                environment::reload_host_software(connection)
            })
            .enabled(connected),
        ),
        update(connection),
        update_channel(connection),
        ui::setting(
            tr("表示言語"),
            || tr("アプリで使用する言語を選択します。").to_owned(),
            language(connection),
        ),
        ui::setting(
            tr("送信ショートカット"),
            || tr("Enter でプロンプトを送信するか改行するかを選択します。").to_owned(),
            send_shortcut(connection),
        ),
        ui::setting(
            tr("フォローアップの動作"),
            move || follow_up_description(preferences(connection).send_shortcut),
            follow_up(connection),
        ),
    ])
}

fn preferences(connection: Connection) -> ComposerPreferences {
    connection
        .composer
        .with(|c| c.controls.follow_up.preferences)
}

fn set_preferences(connection: Connection, change: impl FnOnce(&mut ComposerPreferences)) {
    let mut preferences = connection
        .composer
        .with_untracked(|c| c.controls.follow_up.preferences);
    change(&mut preferences);
    connection.dispatch(Intent::SetComposerPreferences { preferences });
}

/// What Enter does in the composer (T3 Code's send shortcut).
fn send_shortcut(connection: Connection) -> ui::Control {
    const SHORTCUTS: [SendShortcut; 3] = [
        SendShortcut::Enter,
        SendShortcut::ModEnterMultiline,
        SendShortcut::ModEnter,
    ];
    let name = |shortcut| match shortcut {
        SendShortcut::Enter => "Enter".to_owned(),
        SendShortcut::ModEnterMultiline => tr_format(
            "{0} + Enter（複数行のとき）",
            [&crate::composer::modifier()],
        ),
        SendShortcut::ModEnter => tr_format("常に {0} + Enter", [&crate::composer::modifier()]),
    };
    let current = move || preferences(connection).send_shortcut;
    choice(
        move || name(current()),
        move || {
            json!(SHORTCUTS.map(|shortcut| json!({
                "id": name(shortcut),
                "title": name(shortcut),
                "selected": shortcut == current(),
            })))
        },
        move |id: &str| {
            if let Some(shortcut) = SHORTCUTS.into_iter().find(|shortcut| name(*shortcut) == id) {
                set_preferences(connection, |preferences| {
                    preferences.send_shortcut = shortcut
                });
            }
        },
    )
}

fn follow_up_description(shortcut: SendShortcut) -> String {
    let modifier = crate::composer::modifier();
    let opposite = match shortcut {
        SendShortcut::ModEnterMultiline => tr_format(
            "1行のプロンプトは {0} + Enter、複数行のプロンプトは {0} + Shift + Enter で、そのメッセージだけ逆の動作になります。",
            [&modifier],
        ),
        SendShortcut::ModEnter => tr_format(
            "{0} + Shift + Enter で、そのメッセージだけ逆の動作になります。",
            [&modifier],
        ),
        SendShortcut::Enter => tr_format(
            "{0} + Enter で、そのメッセージだけ逆の動作になります。",
            [&modifier],
        ),
    };
    format!(
        "{}{opposite}",
        tr(
            "エージェントの実行中に送るメッセージを、キューに追加するか実行中の作業に追加で指示するかを選択します。"
        )
    )
}

/// What a message sent while the agent works does (T3 Code's follow-up behavior).
fn follow_up(connection: Connection) -> ui::Control {
    const BEHAVIORS: [FollowUp; 2] = [FollowUp::Queue, FollowUp::Steer];
    let name = |behavior| match behavior {
        FollowUp::Queue => tr("キュー"),
        FollowUp::Steer => tr("追加の指示"),
    };
    let current = move || preferences(connection).follow_up;
    choice(
        move || name(current()).to_owned(),
        move || {
            json!(BEHAVIORS.map(|behavior| json!({
                "id": name(behavior),
                "title": name(behavior),
                "selected": behavior == current(),
            })))
        },
        move |id: &str| {
            if let Some(behavior) = BEHAVIORS.into_iter().find(|behavior| name(*behavior) == id) {
                set_preferences(connection, |preferences| preferences.follow_up = behavior);
            }
        },
    )
}

fn update_phase(connection: Connection) -> Memo<UpdatePhase> {
    computed(move || {
        connection.state.with(|state| {
            state
                .update
                .as_ref()
                .map_or(UpdatePhase::Development, |update| update.phase.clone())
        })
    })
}

/// This app's update: where it is, beside the one action it allows next.
fn update(connection: Connection) -> View {
    let phase = update_phase(connection);
    let status = move || {
        phase.with(|phase| match phase {
            UpdatePhase::Development => tr("開発ビルドでは更新を確認しません。").to_owned(),
            UpdatePhase::Checking => tr("更新を確認中…").to_owned(),
            UpdatePhase::Current => tr("最新のバージョンです。").to_owned(),
            UpdatePhase::Available(download) => {
                tr_format("Bex {0}をダウンロードします。", [&download.build])
            }
            UpdatePhase::Downloading(percent) => tr_format("ダウンロード中 {0}%", [percent]),
            UpdatePhase::Ready(build) => tr_format(
                "再起動してBex {0}に更新します。中断した会話は同じ会話で再開します。",
                [build],
            ),
            UpdatePhase::Installing => tr("再起動の準備中…").to_owned(),
            UpdatePhase::Failed(reason) => localized(reason.clone()),
        })
    };
    let action = move || {
        SharedString::from(phase.with(|phase| match phase {
            UpdatePhase::Available(_) | UpdatePhase::Downloading(_) => tr("ダウンロード"),
            UpdatePhase::Ready(_) | UpdatePhase::Installing => tr("再起動して更新"),
            _ => tr("アップデートを確認"),
        }))
    };
    ui::setting(
        tr("アップデート"),
        status,
        ui::text_button(action, move || {
            connection.application.with(|app| app.advance_update())
        })
        .enabled(move || phase.with(|phase| !phase.busy() && *phase != UpdatePhase::Development)),
    )
}

/// The releases this app receives. A staged build belongs to its channel.
fn update_channel(connection: Connection) -> View {
    const CHANNELS: [Channel; 2] = [Channel::Production, Channel::Nightly];
    let name = |channel| match channel {
        Channel::Production => tr("安定版"),
        Channel::Nightly => "Nightly",
    };
    let channel = computed(move || {
        connection
            .state
            .with(|state| state.update.as_ref().map(|update| update.channel))
            .unwrap_or_default()
    });
    let phase = update_phase(connection);
    let items = move || {
        json!(CHANNELS.map(|value| json!({
            "id": name(value),
            "title": name(value),
            "selected": value == channel.get(),
        })))
    };
    ui::setting(
        tr("更新チャンネル"),
        || tr("このPCのBexが受け取るリリースを選択します。").to_owned(),
        choice(
            move || name(channel.get()).to_owned(),
            items,
            move |id: &str| {
                if let Some(value) = CHANNELS.into_iter().find(|value| name(*value) == id) {
                    connection
                        .application
                        .with(|app| app.select_update_channel(value));
                }
            },
        )
        .enabled(move || {
            phase.with(|phase| {
                !phase.busy() && !matches!(phase, UpdatePhase::Development | UpdatePhase::Ready(_))
            })
        }),
    )
}

/// The UI language. Choosing one makes the whole window again in it.
pub(crate) fn language(connection: Connection) -> ui::Control {
    const LANGUAGES: [(UiLanguage, &str); 2] = [
        (UiLanguage::Japanese, "日本語"),
        (UiLanguage::English, "English"),
    ];
    let current = l10n::language();
    let name = move || {
        LANGUAGES
            .into_iter()
            .find_map(|(language, name)| (language == current).then(|| name.to_owned()))
            .unwrap_or_default()
    };
    let items = move || {
        json!(
            LANGUAGES
                .map(|(language, name)| json!({"id": language.code(), "title": name, "selected": language == current}))
        )
    };
    choice(name, items, move |id: &str| {
        if let Some((language, _)) = LANGUAGES
            .into_iter()
            .find(|(language, _)| language.code() == id)
        {
            connection
                .application
                .with(|app| app.select_language(language));
        }
    })
}

/// A popup button: the current choice, opening its menu.
fn choice(
    label: impl Fn() -> String + Copy + 'static,
    items: impl Fn() -> serde_json::Value + Copy + 'static,
    select: impl Fn(&str) + Clone + 'static,
) -> ui::Control {
    ui::text_button(
        move || SharedString::from(format!("{} ▾", label())),
        move || ui::popup(&items(), select.clone()),
    )
}

/// The Host's providers beside the selected one's accounts.
fn providers(connection: Connection) -> View {
    let selected = computed(move || {
        connection
            .state
            .with(|state| state.provider)
            .unwrap_or(ProviderKind::Codex)
    });
    let select = move |provider| {
        connection
            .application
            .with(|app| app.navigate(Navigation::Provider(Some(provider))))
    };
    view()
        .gap_4()
        .child(
            view().flex_row().justify_end().child(
                ui::text_button(tr("状態を確認"), move || {
                    environment::reload_providers(connection)
                })
                .enabled(move || environment::providers_reloadable(connection)),
            ),
        )
        .child(
            view()
                .flex_row()
                .items_start()
                .gap_6()
                .child(
                    view()
                        .w(px(250.0))
                        .flex_shrink_0()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(colors::BORDER)
                        .bg(colors::SURFACE)
                        .overflow_hidden()
                        .child(each(
                            move || environment::agents(connection),
                            |agent: &ConnectionAgent| agent.provider,
                            move |agent, _| provider_row(connection, agent, selected, select),
                        )),
                )
                .child(view().flex_1().min_w_0().child(each(
                    move || vec![selected.get()],
                    |provider: &ProviderKind| *provider,
                    |provider, _| crate::accounts::screen(provider.get_untracked()),
                ))),
        )
        .child(
            text(environment::providers_note())
                .text_size(px(12.0))
                .text_color(colors::MUTED),
        )
}

fn provider_row(
    connection: Connection,
    agent: ReadSignal<ConnectionAgent>,
    selected: Memo<ProviderKind>,
    select: impl Fn(ProviderKind) + 'static,
) -> View {
    let provider = agent.with_untracked(|a| a.provider);
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .p_3()
        .border_b_1()
        .border_b_color(colors::BORDER)
        .style_when(
            move || selected.get() == provider,
            |s| s.bg(colors::SELECTED),
        )
        .child(
            nsh_base::button()
                .flex_1()
                .min_w_0()
                .flex_row()
                .items_center()
                .gap_2()
                .on_press(move || select(provider))
                .child(match crate::model_controls::provider_mark(provider.key()) {
                    Some((markup, color)) => svg(ImageSrc::svg_markup(markup))
                        .w(px(16.0))
                        .h(px(16.0))
                        .text_color(color)
                        .into_element(),
                    None => view().into_element(),
                })
                .child(
                    view()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            text(move || agent.with(|a| a.name.clone()))
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(
                            text(move || {
                                agent.with(|a| {
                                    a.error
                                        .as_ref()
                                        .map(ToString::to_string)
                                        .unwrap_or_else(|| a.label.clone())
                                })
                            })
                            .text_size(px(12.0))
                            .text_color(colors::MUTED),
                        ),
                ),
        )
        .child(environment::provider_switch(connection, agent))
}

fn environments(connection: Connection, hosts: Memo<Vec<Host>>) -> View {
    view()
        .gap_8()
        .child(ui::card(vec![
            ui::setting(
                move || SharedString::from(environment::host_name(connection)),
                move || environment::connection_status(connection),
                environment::connection_switch(connection),
            ),
            ui::setting(
                tr("プロジェクトの取り込み"),
                || {
                    tr("CodexとClaude Codeの履歴から選べます。元の会話をそのまま再開できます。")
                        .to_owned()
                },
                ui::text_button(
                    tr("既存のプロジェクトを取り込む"),
                    move || {
                        connection.application.with(|app| {
                            app.navigate(Navigation::Settings(false));
                            app.open_import();
                        })
                    },
                )
                .enabled(move || connection.settings.with(|p| p.connected())),
            ),
        ]))
        .child(crate::connections::screen(hosts))
        // Phones reach this PC's own Host.
        .child(when(
            move || hosts.with(|hosts| hosts.iter().any(|host| host.id == crate::client::LOCAL)),
            crate::devices::section,
        ))
}
