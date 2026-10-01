use super::view::section_heading;
use crate::{Runtime, store_session::StoreSession};
use agent_core::{
    presentation::connections::{AgentAvailability, ConnectionAgent},
    state::{Intent, Snapshot, operations as op},
    store::Outcome,
};
use agent_protocol::models::{Invitation, RemoteHost};
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        input::{Input, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        *,
    },
    prelude::FluentBuilder,
    *,
};
use std::{io::Write, sync::Arc};

pub(super) enum HostEvent {
    Selected(Option<RemoteHost>),
    Removed(String),
    SetupAgent(agent_protocol::session::ProviderKind, AgentAvailability),
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionLayout {
    Onboarding,
    Settings,
}
enum Action {
    None,
    Invite,
    Pair,
    Remove(String),
}
enum Update {
    Connected(Result<StoreSession, String>),
    Snapshot,
    Completed(Action, Result<Outcome, String>),
    SshInvitation(Result<Invitation, String>),
}

/// Local management is a separate view/session from the selected conversation
/// host. The parent borrows this snapshot for the host menu, never its connection.
pub(super) struct Hosts {
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<Update>,
    pairing: Entity<TextareaState>,
    ssh: Entity<InputState>,
    adding_remote: bool,
    use_ssh: bool,
    invitation: Option<(String, tempfile::NamedTempFile)>,
    busy: bool,
    connecting: bool,
    error: Option<String>,
}
impl EventEmitter<HostEvent> for Hosts {}
impl Hosts {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        StoreSession::on_app_quit(cx, |view| &mut view.session);
        let (updates, incoming) = async_channel::unbounded();
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(update) = incoming.recv().await {
                if view
                    .update_in(cx, |view, window, cx| view.receive(update, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let mut view = Self {
            session: None,
            snapshot: Arc::default(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            pairing: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("相手の端末で発行した招待を貼り付け")
                    .auto_grow(3, 6)
            }),
            ssh: cx.new(|cx| InputState::new(window, cx).placeholder("user@your-server")),
            adding_remote: false,
            use_ssh: true,
            invitation: None,
            busy: false,
            connecting: false,
            error: None,
        };
        view.connect();
        view
    }
    fn connect(&mut self) {
        if self.connecting {
            return;
        }
        self.connecting = true;
        self.error = None;
        self.session.take();
        let updates = self.updates.clone();
        let connections = self.runtime.connections.clone();
        let runtime = self.runtime.clone();
        self.runtime.handle.spawn(async move {
            StoreSession::publish(
                connections.connect(None, Snapshot::default()).await,
                runtime,
                updates,
                Update::Connected,
                |_| Update::Snapshot,
            )
            .await;
        });
    }
    pub(super) fn refresh(&mut self) {
        if self.busy || self.connecting {
            return;
        }
        if self.snapshot.connected {
            self.dispatch(
                Intent::LoadHostManagement(op::LoadHostManagement {}),
                Action::None,
            );
        } else {
            self.connect();
        }
    }
    fn dispatch(&mut self, intent: Intent, action: Action) {
        let Some(store) = self.session.as_ref().map(|session| &session.store) else {
            return;
        };
        self.busy = true;
        self.error = None;
        let receipt = store.dispatch(intent);
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            let result = receipt.await.map_err(|error| error.to_string());
            let _ = updates.send(Update::Completed(action, result)).await;
        });
    }
    fn receive(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        match update {
            Update::SshInvitation(result) => {
                self.busy = false;
                match result {
                    Ok(invitation) => self.pair(invitation),
                    Err(error) => self.error = Some(error),
                }
            }
            Update::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok(store) => {
                        self.snapshot = store.store.snapshot();
                        self.session = Some(store);
                        self.dispatch(
                            Intent::LoadHostManagement(op::LoadHostManagement {}),
                            Action::None,
                        );
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Update::Snapshot => {
                if let Some(store) = self.session.as_ref().map(|session| &session.store) {
                    self.snapshot = store.snapshot();
                }
            }
            Update::Completed(action, result) => {
                self.busy = false;
                if let Some(store) = self.session.as_ref().map(|session| &session.store) {
                    self.snapshot = store.snapshot();
                }
                match result {
                    Err(error) => self.error = Some(error),
                    Ok(outcome) => match action {
                        Action::Invite => {
                            if let Some(invitation) = &self.snapshot.management.invitation {
                                let result = (|| -> Result<_, String> {
                                    let text = serde_json::to_string(invitation)
                                        .map_err(|error| error.to_string())?;
                                    let code = qrcode::QrCode::with_error_correction_level(
                                        text.as_bytes(),
                                        qrcode::EcLevel::M,
                                    )
                                    .map_err(|error| error.to_string())?;
                                    let svg = code
                                        .render::<qrcode::render::svg::Color>()
                                        .min_dimensions(320, 320)
                                        .build();
                                    let mut file = tempfile::Builder::new()
                                        .prefix("bex-invitation-")
                                        .suffix(".svg")
                                        .tempfile()
                                        .map_err(|error| error.to_string())?;
                                    file.write_all(svg.as_bytes())
                                        .map_err(|error| error.to_string())?;
                                    Ok((text, file))
                                })();
                                match result {
                                    Ok(invitation) => self.invitation = Some(invitation),
                                    Err(error) => self.error = Some(error),
                                }
                            }
                        }
                        Action::Pair => {
                            self.adding_remote = false;
                            self.pairing
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            if let Outcome::RemoteHostPaired { id } = outcome
                                && let Some(host) = self
                                    .snapshot
                                    .management
                                    .remotes
                                    .iter()
                                    .find(|host| host.id == id)
                            {
                                cx.emit(HostEvent::Selected(Some(host.clone())));
                            }
                        }
                        Action::Remove(id) => cx.emit(HostEvent::Removed(id)),
                        Action::None => {}
                    },
                }
            }
        }
        cx.notify();
    }
    fn pair(&mut self, invitation: Invitation) {
        self.dispatch(
            Intent::PairRemoteHost(op::PairRemoteHost {
                invitation,
                name: "Bex Desktop".into(),
            }),
            Action::Pair,
        );
    }

    pub(super) fn connection_choices(
        &self,
        layout: ConnectionLayout,
        current: Option<&str>,
        connected: bool,
        agents: &[ConnectionAgent],
        cx: &Context<Self>,
    ) -> Div {
        let cards = layout == ConnectionLayout::Onboarding;
        let mut choices = v_flex()
            .when(cards, |view| view.gap_3())
            .when(!cards, |view| {
                view.rounded(px(10.))
                    .border_1()
                    .border_color(rgb(0x2b2f35))
                    .overflow_hidden()
            });
        for (index, remote) in std::iter::once(None)
            .chain(self.snapshot.management.remotes.iter().map(Some))
            .enumerate()
        {
            let selected = current == remote.map(|host| host.id.as_str());
            let destination = remote.cloned();
            let name = remote.map_or("このPC", |host| host.name.as_str());
            let label = if selected {
                if connected {
                    "接続済み"
                } else {
                    "接続中…"
                }
            } else if remote.is_none() && self.snapshot.connected {
                "利用可能"
            } else {
                "保存済み"
            };
            let mut card = v_flex()
                .gap_3()
                .p_4()
                .when(cards, |view| {
                    view.rounded(px(12.))
                        .border_1()
                        .border_color(if selected {
                            rgb(0x476ce0)
                        } else {
                            rgb(0x303030)
                        })
                        .bg(rgb(0x151515))
                })
                .when(!cards, |view| view.border_b_1().border_color(rgb(0x2b2f35)))
                .child(
                    h_flex()
                        .items_center()
                        .gap_3()
                        .child(
                            Button::new(format!("onboarding-host-{index}"))
                                .accessibility_label(name.to_owned())
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .child(if remote.is_some() {
                                            Icon::new(IconName::Network)
                                        } else {
                                            Icon::default().path("bex/monitor.svg")
                                        })
                                        .child(name.to_owned()),
                                )
                                .ghost()
                                .flex_1()
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(HostEvent::Selected(destination.clone()));
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if selected && connected {
                                    rgb(0x88c9a0)
                                } else {
                                    rgb(0x949494)
                                })
                                .child(label),
                        )
                        .when_some(remote.filter(|_| !cards), |row, remote| {
                            let id = remote.id.clone();
                            row.child(
                                Button::new(format!("remove-{id}"))
                                    .icon(IconName::Close)
                                    .small()
                                    .ghost()
                                    .accessibility_label("保存した接続を削除")
                                    .tooltip("保存した接続を削除")
                                    .disabled(self.busy || !self.snapshot.connected)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.dispatch(
                                            Intent::RemoveRemoteHost(op::RemoveRemoteHost {
                                                id: id.clone(),
                                            }),
                                            Action::Remove(id.clone()),
                                        );
                                        cx.notify();
                                    })),
                            )
                        }),
                );
            if selected {
                let mut details = v_flex()
                    .gap_2()
                    .when(cards, |view| {
                        view.pt_3().border_t_1().border_color(rgb(0x292929))
                    })
                    .when(!cards, |view| {
                        view.ml_8().p_3().rounded(px(6.)).bg(rgb(0x191c20))
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x949494))
                            .child("この環境で使えるAI"),
                    );
                for agent in agents {
                    let provider = agent.provider;
                    let availability = agent.availability;
                    let mut row = h_flex()
                        .items_center()
                        .gap_3()
                        .py_1()
                        .child(div().flex_1().child(agent.name.clone()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(if availability == AgentAvailability::Ready {
                                    rgb(0x88c9a0)
                                } else {
                                    rgb(0x949494)
                                })
                                .child(agent.label.clone()),
                        );
                    if matches!(
                        availability,
                        AgentAvailability::LoginRequired | AgentAvailability::Unavailable
                    ) {
                        row = row.child(
                            Button::new(format!("onboarding-agent-{index}-{}", agent.name))
                                .label(if availability == AgentAvailability::LoginRequired {
                                    "ログイン"
                                } else {
                                    "設定"
                                })
                                .small()
                                .ghost()
                                .disabled(!connected)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(HostEvent::SetupAgent(provider, availability));
                                })),
                        );
                    }
                    details = details.child(row);
                }
                card = card.child(details);
            }
            choices = choices.child(card);
        }
        choices = choices.child(
            Button::new("onboarding-add-remote")
                .accessibility_label(if cards {
                    "クラウドVM・別のPCに接続"
                } else {
                    "接続先を追加"
                })
                .child(
                    h_flex()
                        .when(!cards, |row| row.w_full())
                        .gap_2()
                        .child(Icon::new(if cards {
                            IconName::Network
                        } else {
                            IconName::Plus
                        }))
                        .child(if cards {
                            "クラウドVM・別のPCに接続"
                        } else {
                            "接続先を追加"
                        }),
                )
                .ghost()
                .w_full()
                .when(!cards, |button| {
                    button.h(px(48.)).px_4().text_color(rgb(0x8bb7f9))
                })
                .disabled(self.busy)
                .on_click(cx.listener(|view, _, _, cx| {
                    view.adding_remote = !view.adding_remote;
                    view.error = None;
                    cx.notify();
                })),
        );
        if self.adding_remote {
            let disabled = self.busy || !self.snapshot.connected;
            let mut methods = h_flex().gap_2();
            for (id, label, use_ssh) in [
                ("onboarding-use-ssh", "SSHで接続", true),
                ("onboarding-use-invitation", "接続情報を貼り付け", false),
            ] {
                methods = methods.child(
                    Button::new(id)
                        .label(label)
                        .small()
                        .ghost()
                        .selected(self.use_ssh == use_ssh)
                        .disabled(disabled)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.use_ssh = use_ssh;
                            view.error = None;
                            cx.notify();
                        })),
                );
            }
            let mut form = v_flex()
                .gap_3()
                .p_4()
                .rounded(px(12.))
                .border_1()
                .border_color(rgb(0x303030))
                .bg(rgb(0x151515))
                .when(!cards, |view| view.mx_4().mb_4())
                .child(methods);
            if self.use_ssh {
                form =
                    form.child("SSH接続先")
                        .child(
                            Input::new(&self.ssh)
                                .aria_label("SSH接続先")
                                .disabled(disabled),
                        )
                        .child(div().text_xs().text_color(rgb(0x949494)).child(
                            "SSHの鍵で接続でき、Bex Hostを起動済みのサーバーにつなぎます。",
                        ));
            } else {
                form = form.child(
                    Textarea::new(&self.pairing)
                        .aria_label("接続情報")
                        .readonly(disabled),
                );
            }
            choices =
                choices.child(
                    form.child(
                        Button::new("onboarding-pair")
                            .label(if self.busy { "接続中…" } else { "接続" })
                            .disabled(disabled)
                            .on_click(cx.listener(|view, _, _, cx| {
                                if view.use_ssh {
                                    view.busy = true;
                                    view.error = None;
                                    let destination = view.ssh.read(cx).value().to_string();
                                    let updates = view.updates.clone();
                                    view.runtime.handle.spawn(async move {
                                        let result =
                                            crate::platform::ssh_invitation(&destination).await;
                                        let _ = updates.send(Update::SshInvitation(result)).await;
                                    });
                                } else {
                                    match serde_json::from_str::<Invitation>(
                                        view.pairing.read(cx).value().as_ref(),
                                    ) {
                                        Ok(invitation) => view.pair(invitation),
                                        Err(_) => view.error = Some(
                                            "接続先で発行したBexの接続情報を貼り付けてください。"
                                                .into(),
                                        ),
                                    }
                                }
                                cx.notify();
                            })),
                    ),
                );
        }
        choices = choices.child(
            h_flex()
                .items_center()
                .gap_3()
                .p_4()
                .when(cards, |view| view.rounded(px(12.)).border_1())
                .when(!cards, |view| view.border_t_1())
                .border_color(rgb(0x2b2f35))
                .text_color(rgb(0x737373))
                .child(Icon::new(IconName::Network))
                .child(div().flex_1().child("Bexの実行環境"))
                .child(div().text_xs().child("今後対応")),
        );
        if let Some(error) = self.error.as_ref().or(self.snapshot.error.as_ref()) {
            choices = choices.child(
                div()
                    .when(!cards, |view| view.p_4())
                    .text_color(rgb(0xff8e86))
                    .child(agent_core::presentation::error::error_message(error)),
            );
        }
        choices
    }
    pub(super) fn menu(
        owner: &Entity<Self>,
        id: &'static str,
        current: Option<&str>,
        disabled: bool,
        cx: &App,
    ) -> impl IntoElement {
        let view = owner.read(cx);
        let hosts = &view.snapshot.management.remotes;
        let name: SharedString = current
            .and_then(|id| hosts.iter().find(|host| host.id == id))
            .map_or_else(|| "この端末".into(), |host| host.name.clone().into());
        let current = current.map(str::to_owned);
        let hosts = hosts.clone();
        let owner = owner.downgrade();
        Button::new(id)
            .disabled(disabled)
            .label(name)
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, _| {
                let local = owner.clone();
                menu = menu.item(
                    PopupMenuItem::new("この端末")
                        .checked(current.is_none())
                        .on_click(move |_, _, cx| {
                            let _ = local.update(cx, |_, cx| cx.emit(HostEvent::Selected(None)));
                        }),
                );
                for host in &hosts {
                    let owner = owner.clone();
                    let host = host.clone();
                    menu = menu.item(
                        PopupMenuItem::new(host.name.clone())
                            .checked(current.as_deref() == Some(&host.id))
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |_, cx| {
                                    cx.emit(HostEvent::Selected(Some(host.clone())))
                                });
                            }),
                    );
                }
                menu
            })
    }
}
impl Render for Hosts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = !self.snapshot.connected || self.busy;
        let mut body = v_flex().w_full().gap_5();
        if !self.snapshot.connected {
            return body
                .child(
                    div()
                        .text_color(rgb(0x949ca8))
                        .child("このPCの接続情報を読み込めません。"),
                )
                .child(
                    Button::new("connect-host")
                        .label(if self.connecting {
                            "接続中…"
                        } else {
                            "Host に接続"
                        })
                        .disabled(self.connecting)
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.connect();
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        body = body.child(
            h_flex()
                .items_center()
                .flex_wrap()
                .gap_4()
                .child(
                    section_heading(
                        "スマートフォンから接続",
                        "このPCの会話を、iPhoneやAndroidから続けられます。",
                    )
                    .flex_1()
                    .min_w(px(220.)),
                )
                .child(
                    Button::new("invite")
                        .label("接続用QRを表示")
                        .icon(Icon::default().path("bex/qr-code.svg"))
                        .primary()
                        .disabled(disabled)
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.dispatch(
                                Intent::CreateInvitation(op::CreateInvitation {}),
                                Action::Invite,
                            );
                            cx.notify();
                        })),
                ),
        );
        if let Some((_, file)) = &self.invitation {
            body = body.child(
                h_flex().flex_wrap().gap_5().p_4().rounded(px(10.))
                    .border_1().border_color(rgb(0x2b2f35))
                    .child(img(file.path().to_path_buf()).w(px(200.)).h(px(200.)))
                    .child(v_flex().flex_1().min_w(px(200.)).gap_3()
                        .child(div().font_semibold().child("BexアプリでQRを読み取る"))
                        .child(div().text_sm().text_color(rgb(0x949ca8))
                            .child("1回限りの招待です。QRを読み取るか、招待をコピーして相手の端末に貼り付けてください。"))
                        .child(Button::new("copy-invite")
                            .label("招待をコピー")
                            .icon(IconName::Copy)
                            .ghost()
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some((text, _)) = &view.invitation {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                }
                            })))),
            );
        }
        let mut devices = v_flex()
            .gap_3()
            .pt_6()
            .border_t_1()
            .border_color(rgb(0x2b2f35))
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().font_semibold().child("接続を許可した端末"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x949ca8))
                                    .child("このPCへアクセスできる端末を管理します。"),
                            ),
                    )
                    .child(
                        Button::new("refresh-hosts")
                            .icon(IconName::RotateCw)
                            .small()
                            .ghost()
                            .accessibility_label("接続一覧を更新")
                            .tooltip("接続一覧を更新")
                            .disabled(disabled)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.refresh();
                                cx.notify();
                            })),
                    ),
            );
        if let Some(status) = &self.snapshot.management.status {
            let mut list = v_flex()
                .rounded(px(10.))
                .border_1()
                .border_color(rgb(0x2b2f35))
                .overflow_hidden();
            if status.devices.is_empty() {
                list = list.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(rgb(0x949ca8))
                        .child("接続を許可した端末はありません。"),
                );
            }
            for (index, node) in status.devices.iter().enumerate() {
                let node = node.clone();
                list = list.child(
                    h_flex()
                        .items_center()
                        .gap_4()
                        .p_4()
                        .when(index > 0, |row| {
                            row.border_t_1().border_color(rgb(0x2b2f35))
                        })
                        .child(Icon::new(IconName::Network).text_color(rgb(0x949ca8)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(format!("端末 {}", index + 1))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x949ca8))
                                        .text_ellipsis()
                                        .child(node.clone()),
                                ),
                        )
                        .child(
                            Button::new(format!("revoke-{index}"))
                                .label("接続を解除")
                                .small()
                                .ghost()
                                .text_color(rgb(0x8bb7f9))
                                .disabled(disabled)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.dispatch(
                                        Intent::RevokeDevice(op::RevokeDevice { id: node.clone() }),
                                        Action::None,
                                    );
                                    cx.notify();
                                })),
                        ),
                );
            }
            devices = devices.child(list);
        } else {
            devices = devices.child(
                div()
                    .text_sm()
                    .text_color(rgb(0x949ca8))
                    .child("端末を読み込み中…"),
            );
        }
        body.child(devices).into_any_element()
    }
}
