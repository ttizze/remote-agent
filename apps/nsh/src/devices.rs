//! This PC's Host as other devices reach it: the invitation a phone scans or
//! pastes, and the devices it allowed.
use crate::l10n::tr;
use crate::ui::{self, Enabled, Icon, colors};
use crate::{Connection, client::LOCAL};
use agent_core::{
    presentation::host_management::invitation_code,
    state::{Intent, operations as op},
};
use agent_protocol::models::HostDevice;
use nsh_core::{element::ImageSrc, prelude::*};

/// A shown invitation: the text a phone reads, and its QR code.
#[derive(Clone, PartialEq)]
struct Shown {
    code: String,
    svg: String,
}

pub(crate) fn section() -> View {
    let connection = use_context::<Connection>().unwrap();
    let local = computed(move || {
        connection
            .state
            .with(|state| state.publications.get(LOCAL).cloned())
    });
    let connected = computed(move || {
        local.with(|value| {
            value
                .as_ref()
                .is_some_and(|value| value.snapshot.connection.connected)
        })
    });
    let devices = computed(move || {
        local.with(|value| {
            value.as_ref().and_then(|value| {
                value
                    .snapshot
                    .connection
                    .management
                    .status
                    .as_ref()
                    .map(|status| status.devices.clone())
            })
        })
    });
    let load = move || {
        connection.client.with(|client| {
            client.dispatch(
                LOCAL,
                None,
                Intent::LoadHostManagement(op::LoadHostManagement {}),
            )
        })
    };
    effect(move || {
        if connected.get() {
            load();
        }
    });
    let shown = signal(None::<Shown>);
    let creating = signal(false);
    let error = signal(None::<String>);
    let invite = move || {
        if creating.get_untracked() {
            return;
        }
        creating.set(true);
        error.set(None);
        let response = connection.client.with(|client| {
            client.request(
                LOCAL,
                None,
                Intent::CreateInvitation(op::CreateInvitation {}),
            )
        });
        spawn(async move {
            // A failed request shows as the Host's notice.
            if let Ok(Ok(_)) = response.recv().await
                && let Some(invitation) = connection.client.with(|client| {
                    client
                        .snapshot(LOCAL)
                        .and_then(|snapshot| snapshot.connection.management.invitation.clone())
                })
            {
                match qr(invitation_code(&invitation)) {
                    Ok(value) => shown.set(Some(value)),
                    Err(reason) => error.set(Some(reason)),
                }
            }
            creating.set(false);
        });
    };
    view()
        .gap_4()
        .child(ui::card(vec![ui::setting(
            tr("スマートフォンから接続"),
            || tr("このPCの会話を、iPhoneやAndroidから続けられます。").to_owned(),
            ui::text_button(tr("接続用QRを表示"), invite)
                .enabled(move || connected.get() && !creating.get()),
        )]))
        .child(when(
            move || shown.with(Option::is_some),
            move || invitation(shown),
        ))
        .child(text(move || error.get().unwrap_or_default()).selectable(true))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    view()
                        .flex_1()
                        .gap_1()
                        .child(
                            text(tr("接続を許可した端末"))
                                .text_size(px(16.0))
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(
                            text(tr("このPCへアクセスできる端末を管理します。"))
                                .text_size(px(13.0))
                                .text_color(colors::muted()),
                        ),
                )
                .child(
                    ui::icon_button(Icon::REFRESH, tr("接続一覧を更新"), load)
                        .enabled(move || connected.get()),
                ),
        )
        .child(
            view()
                .rounded(px(12.0))
                .border_1()
                .border_color(colors::border())
                .bg(colors::SURFACE)
                .child(when(
                    move || devices.with(|devices| devices.as_ref().is_none_or(Vec::is_empty)),
                    move || {
                        text(move || {
                            if devices.with(Option::is_none) {
                                tr("端末を読み込み中…")
                            } else {
                                tr("接続を許可した端末はありません。")
                            }
                        })
                        .px_5()
                        .py_4()
                        .text_size(px(13.0))
                        .text_color(colors::muted())
                    },
                ))
                .child(each(
                    move || devices.get().unwrap_or_default(),
                    |device: &HostDevice| device.id.clone(),
                    move |device, index| {
                        let device = device.get_untracked();
                        let id = device.id.clone();
                        ui::setting(
                            device
                                .name
                                .clone()
                                .unwrap_or_else(|| tr("名前未取得の端末").into()),
                            move || device.id.clone(),
                            ui::text_button(tr("接続を解除"), move || {
                                connection.client.with(|client| {
                                    client.dispatch(
                                        LOCAL,
                                        None,
                                        Intent::RevokeDevice(op::RevokeDevice { id: id.clone() }),
                                    )
                                })
                            })
                            .enabled(move || connected.get()),
                        )
                        .style_when(
                            move || index.get() > 0,
                            |s| s.border_t_1().border_t_color(colors::border()),
                        )
                    },
                )),
        )
}

/// The one-time invitation: its QR code beside copying it.
fn invitation(shown: Signal<Option<Shown>>) -> View {
    view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_5()
        .p_5()
        .rounded(px(12.0))
        .border_1()
        .border_color(colors::border())
        .child(
            svg(move || {
                shown.with(|value| {
                    ImageSrc::svg_markup(value.as_ref().map_or("", |value| &value.svg))
                })
            })
                .w(px(200.0))
                .h(px(200.0))
                .flex_shrink_0(),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(200.0))
                .gap_3()
                .child(
                    text(tr("BexアプリでQRを読み取る"))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    text(tr("1回限りの招待です。QRを読み取るか、招待をコピーして相手の端末に貼り付けてください。"))
                        .text_size(px(13.0))
                        .text_color(colors::muted()),
                )
                .child(view().flex_row().child(ui::text_button(tr("招待をコピー"), move || {
                    set_clipboard(shown.with_untracked(|value| {
                        value.as_ref().map(|value| value.code.clone()).unwrap_or_default()
                    }))
                }))),
        )
}

/// The code a phone's camera reads, in the size it reads well.
fn qr(code: String) -> Result<Shown, String> {
    let svg = qrcode::QrCode::with_error_correction_level(code.as_bytes(), qrcode::EcLevel::M)
        .map_err(|reason| reason.to_string())?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .build();
    Ok(Shown { code, svg })
}
