use crate::l10n::{localized, tr, tr_format};
use crate::{Connection, application::Host, native};
use nsh_core::prelude::*;
use serde_json::json;

pub(crate) fn selector(
    hosts: Memo<Vec<Host>>,
    selected: Memo<String>,
) -> nsh_core::element::NativeView {
    let connection = use_context::<Connection>().unwrap();
    native::menu(
        "desktopcomputer",
        tr("接続先"),
        move || {
            let all = connection.state.with(|state| state.all_hosts);
            let mut entries = vec![
                json!({"id":"", "title":tr("すべての接続先"), "symbol":"square.stack", "selected":all}),
                json!({"separator":true}),
            ];
            entries.extend(hosts.with(|hosts| hosts.iter().map(|host| json!({"id":host.id,"title":host.label(),"selected":!all && host.id == selected.get()})).collect::<Vec<_>>()));
            json!(entries)
        },
        move |name| {
            connection.application.with(|application| {
                if name.is_empty() { application.select_all_hosts(); }
                else { application.select_host(name.to_owned()); }
            });
        },
    )
    .prop("title", move || {
        if connection.state.with(|state| state.all_hosts) { return json!(tr("すべての接続先")); }
        json!(hosts.with(|hosts| {
            hosts
                .iter()
                .find(|host| host.id == selected.get())
                .map(|host| {
                    if is_desktop() && host.id == "local" {
                        "Local".into()
                    } else {
                        host.name.clone()
                    }
                })
                .unwrap_or_else(|| tr("接続先").into())
        }))
    })
    .prop("identifier", json!("connections.list"))
    .min_h(px(if is_desktop() { 28.0 } else { 44.0 }))
}

/// The Hosts this device can work on, and pairing another PC.
pub(crate) fn screen(hosts: Memo<Vec<Host>>) -> impl IntoElement {
    use crate::ui::{self, colors};
    let connection = use_context::<Connection>().unwrap();
    view()
        .gap_4()
        .text_color(move || nsh_base::theme().foreground)
        .child(heading(tr("接続先")))
        .child(
            view()
                .rounded(px(12.0))
                .border_1()
                .border_color(colors::border())
                .bg(colors::SURFACE)
                .child(each(
                    move || hosts.get(),
                    |host: &Host| host.id.clone(),
                    move |host, index| {
                        let host = host.get_untracked();
                        let id = host.id.clone();
                        let removed = host.id.clone();
                        let controls =
                            view()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(ui::text_button(tr("接続"), move || {
                                    connection.application.with(|application| {
                                        application.select_host(id.clone())
                                    });
                                }));
                        ui::setting(
                            host.label(),
                            String::new,
                            // A paired PC can be forgotten; this PC and its
                            // WSL distros are found again.
                            if crate::client::saved(&host.id) {
                                controls.child(ui::icon_button(
                                    ui::Icon::CLOSE,
                                    tr("保存した接続を削除"),
                                    move || {
                                        connection
                                            .client
                                            .with(|client| client.remove(removed.clone()))
                                    },
                                ))
                            } else {
                                controls
                            },
                        )
                        .style_when(
                            move || index.get() > 0,
                            |s| s.border_t_1().border_t_color(colors::border()),
                        )
                    },
                )),
        )
        .child(heading(tr("PCを追加")))
        .child(pairing())
}

/// The desktop's first screen before any Host: T3 Code's hero over pairing a PC.
#[cfg(desktop)]
pub(crate) fn hero() -> impl IntoElement {
    use crate::ui::colors;
    scroll().flex_1().child(
        view().w_full().items_center().px_8().py(px(96.0)).child(
            view()
                .w_full()
                .max_w(px(512.0))
                .gap_6()
                .child(
                    view()
                        .items_center()
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
                            .text_size(px(14.0))
                            .text_color(colors::muted()),
                        ),
                )
                .child(pairing()),
        ),
    )
}

fn heading(title: &'static str) -> impl IntoElement {
    text(title)
        .text_size(px(16.0))
        .font_weight(FontWeight::SEMIBOLD)
}

/// Pairing a PC from the connection information its Bex issued, pasted or, on
/// the desktop, read over the user's SSH: the information, then what this
/// device will send it, then the agreement.
pub(crate) fn pairing() -> impl IntoElement {
    use crate::ui::{self, Enabled, colors};
    let connection = use_context::<Connection>().unwrap();
    let invitation = signal(String::new());
    let confirmation = signal(None::<agent_protocol::models::Invitation>);
    let error = signal(None::<String>);
    let confirmed = move |read: fn(&agent_protocol::models::Invitation) -> String| {
        move || confirmation.with(|value| value.as_ref().map(read).unwrap_or_default())
    };
    let paste = move || {
        view()
            .gap_3()
            .child(
                text(tr("PCのBexで発行した接続情報を貼り付けてください。"))
                    .text_size(px(13.0))
                    .text_color(colors::muted()),
            )
            .child(
                ui::field()
                    .masked(true)
                    .value(move || invitation.get())
                    .on_change(move |text: &str| invitation.set(text.into()))
                    .placeholder(tr("接続情報")),
            )
            .child(view().flex_row().justify_end().child(ui::text_button(
                tr("接続先を確認"),
                move || {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|time| time.as_secs())
                        .unwrap_or_default();
                    match agent_core::client::native::parse_invitation(
                        invitation.get_untracked(),
                        now,
                    ) {
                        Ok(value) => {
                            confirmation.set(Some(value));
                            error.set(None);
                        }
                        Err(reason) => error.set(Some(localized(reason.to_string()))),
                    }
                },
            )))
    };
    #[cfg(desktop)]
    let information = move || ssh_or_paste(paste, confirmation, error);
    #[cfg(not(desktop))]
    let information = paste;
    view()
        .gap_4()
        .child(when(move || confirmation.get().is_none(), move || {
            ui::card(vec![information().px_5().py_4()])
        }))
        .child(when(move || confirmation.get().is_some(), move || {
            ui::card(vec![view()
                .px_5()
                .py_4()
                .gap_3()
                .child(text(confirmed(|value| value.host_name.clone())).font_weight(FontWeight::MEDIUM))
                .child(text(tr("メッセージ・添付ファイル・作業に必要なプロジェクトの内容を、このPCと利用するAIサービスに送信します。")))
                .child(text(confirmed(|value| {
                    let recipients = Some(value.ai_recipients.join(tr("、"))).filter(|value| !value.is_empty()).unwrap_or_else(|| tr("未設定").into());
                    tr_format("AI処理：{0}", [&recipients])
                })))
                .child(text(confirmed(|value| value.transcription_recipient.as_ref().map(|recipient| tr_format("音声入力：{0}（文字起こし）", [recipient])).unwrap_or_default())))
                .child(text(tr("共有PCでは、管理者などが内容を閲覧できる場合があります。")).text_size(px(13.0)).text_color(colors::muted()))
                .child(view().flex_row().justify_end().gap_2()
                    .child(ui::text_button(tr("変更"), move || {
                        confirmation.set(None); invitation.set(String::new());
                    }).enabled(move || !connection.pairing.get()))
                    .child(ui::text_button(tr("同意して接続"), move || {
                        if !connection.pairing.get_untracked() && let Some(invitation) = confirmation.get_untracked() {
                            connection.application.with(|application| application.pair(invitation));
                        }
                    }).enabled(move || !connection.pairing.get())))])
        }))
        .child(text(move || error.get().unwrap_or_default()).selectable(true))
}

/// On the desktop the information can also come from a server over SSH;
/// pasting stays the default.
#[cfg(desktop)]
fn ssh_or_paste(
    paste: impl Fn() -> View + Copy + 'static,
    confirmation: Signal<Option<agent_protocol::models::Invitation>>,
    error: Signal<Option<String>>,
) -> View {
    use crate::ui::{self, Enabled, colors};
    let connection = use_context::<Connection>().unwrap();
    let ssh = signal(false);
    let destination = signal(String::new());
    let reading = signal(false);
    let method = move |ssh: bool| {
        if ssh {
            tr("SSHで接続")
        } else {
            tr("接続情報を貼り付け")
        }
    };
    let read = move || {
        if reading.get_untracked() {
            return;
        }
        reading.set(true);
        error.set(None);
        let answer = connection
            .client
            .with(|client| client.ssh_invitation(destination.get_untracked()));
        spawn(async move {
            match answer.recv().await {
                Ok(Ok(value)) => confirmation.set(Some(value)),
                Ok(Err(reason)) => error.set(Some(reason)),
                Err(_) => {}
            }
            reading.set(false);
        });
    };
    view()
        .gap_3()
        .child(
            view().flex_row().child(
                ui::choice_menu(
                    "network",
                    tr("接続方法"),
                    move || method(ssh.get()).to_owned(),
                    move || {
                        json!([false, true].map(|value| json!({
                            "id": method(value),
                            "title": method(value),
                            "selected": value == ssh.get(),
                        })))
                    },
                    move |id: &str| {
                        ssh.set(id == method(true));
                        error.set(None);
                    },
                )
                .enabled(move || !reading.get()),
            ),
        )
        .child(when(move || !ssh.get(), paste))
        .child(when(
            move || ssh.get(),
            move || {
                view()
                    .gap_3()
                    .child(
                        text(tr(
                            "SSHの鍵で接続でき、Bexのホストを起動済みのサーバーにつなぎます。",
                        ))
                        .text_size(px(13.0))
                        .text_color(colors::muted()),
                    )
                    .child(text(tr("SSH接続先")).text_size(px(13.0)))
                    .child(
                        ui::field()
                            .value(move || destination.get())
                            .on_change(move |text: &str| destination.set(text.into()))
                            .placeholder("user@your-server"),
                    )
                    .child(
                        view().flex_row().justify_end().child(
                            ui::text_button(
                                move || {
                                    SharedString::from(if reading.get() {
                                        tr("接続中…")
                                    } else {
                                        tr("接続")
                                    })
                                },
                                read,
                            )
                            .enabled(move || !reading.get()),
                        ),
                    )
            },
        ))
}
