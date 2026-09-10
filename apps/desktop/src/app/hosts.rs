use crate::Runtime;
use agent_core::state::operations as op;
use agent_core::{
    models::{Invitation, RemoteHost},
    state::{Intent, Snapshot},
    store::{Outcome, Store},
};
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        input::{Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        *,
    },
    *,
};
use std::{io::Write, sync::Arc};

pub(super) enum HostEvent {
    Selected(Option<RemoteHost>),
    Removed(String),
    Changed,
}
enum Action {
    Refresh,
    Invite,
    Pair,
    Remove(String),
    Revoke,
}
enum Update {
    Connected(Result<Arc<Store>, String>),
    Snapshot,
    Completed(Action, Result<Outcome, String>),
}

/// Local management is a separate view/session from the selected conversation
/// host. The parent borrows this snapshot for the host menu, never its connection.
pub(super) struct Hosts {
    store: Option<Arc<Store>>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<Update>,
    pairing: Entity<TextareaState>,
    invitation: Entity<TextareaState>,
    invitation_file: Option<tempfile::NamedTempFile>,
    busy: bool,
    connecting: bool,
    error: Option<String>,
}
impl EventEmitter<HostEvent> for Hosts {}
impl Drop for Hosts {
    fn drop(&mut self) {
        self.close();
    }
}
impl Hosts {
    fn close(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let store = self.store.take()?;
        Some(self.runtime.closing.spawn_on(
            async move {
                let _ = store.close().await;
            },
            &self.runtime.handle,
        ))
    }
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_app_quit(|view, _| {
            if let Some(close) = view.close() {
                let _ = view.runtime.handle.block_on(close);
            }
            async {}
        })
        .detach();
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
            store: None,
            snapshot: Arc::default(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            pairing: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("相手の端末で発行した招待を貼り付け")
                    .auto_grow(3, 6)
            }),
            invitation: cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 6)),
            invitation_file: None,
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
        if let Some(store) = self.store.take() {
            self.runtime.closing.spawn_on(
                async move {
                    let _ = store.close().await;
                },
                &self.runtime.handle,
            );
        }
        let updates = self.updates.clone();
        let connections = self.runtime.connections.clone();
        self.runtime.handle.spawn(async move {
            match connections.connect(None, Snapshot::default()).await {
                Ok(store) => {
                    let mut snapshots = store.subscribe();
                    if updates
                        .send(Update::Connected(Ok(Arc::new(store))))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    loop {
                        snapshots.borrow_and_update();
                        if updates.send(Update::Snapshot).await.is_err()
                            || snapshots.changed().await.is_err()
                        {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = updates.send(Update::Connected(Err(error))).await;
                }
            }
        });
    }
    fn dispatch(&mut self, intent: Intent, action: Action) {
        let Some(store) = &self.store else {
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
            Update::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok(store) => {
                        self.snapshot = store.snapshot();
                        self.store = Some(store);
                        self.dispatch(
                            Intent::LoadHostManagement(op::LoadHostManagement),
                            Action::Refresh,
                        );
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Update::Snapshot => {
                if let Some(store) = &self.store {
                    self.snapshot = store.snapshot();
                }
            }
            Update::Completed(action, result) => {
                self.busy = false;
                if let Some(store) = &self.store {
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
                                    Ok((text, file)) => {
                                        self.invitation.update(cx, |input, cx| {
                                            input.set_value(text, window, cx)
                                        });
                                        self.invitation_file = Some(file);
                                    }
                                    Err(error) => self.error = Some(error),
                                }
                            }
                        }
                        Action::Pair => {
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
                        Action::Refresh | Action::Revoke => {}
                    },
                }
            }
        }
        cx.emit(HostEvent::Changed);
        cx.notify();
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
        let mut body = v_flex()
            .w_full()
            .gap_3()
            .child(div().text_xl().child("端末と接続"));
        if let Some(status) = &self.snapshot.management.status {
            body = body.child(format!("{} · {}", status.name, status.node_id));
        }
        if let Some(error) = self.error.as_ref().or(self.snapshot.error.as_ref()) {
            body = body.child(div().text_color(rgb(0xff8e86)).child(error.clone()));
        }
        if !self.snapshot.connected {
            return body
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
            Button::new("invite")
                .label("別の端末を招待")
                .disabled(disabled)
                .on_click(cx.listener(|view, _, _, cx| {
                    view.dispatch(
                        Intent::CreateInvitation(op::CreateInvitation {}),
                        Action::Invite,
                    );
                    cx.notify();
                })),
        );
        if let Some(file) = &self.invitation_file {
            body = body
                .child(img(file.path().to_path_buf()).w(px(320.)).h(px(320.)))
                .child("1回限りの招待です。相手端末で読み取るか、招待を貼り付けてください。")
                .child(Textarea::new(&self.invitation).readonly(true))
                .child(
                    Button::new("copy-invite")
                        .label("招待をコピー")
                        .on_click(cx.listener(|view, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                view.invitation.read(cx).value().to_string(),
                            ));
                        })),
                );
        }
        if let Some(status) = &self.snapshot.management.status {
            for (index, node) in status.devices.iter().enumerate() {
                let node = node.clone();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().child(node.clone()))
                        .child(
                            Button::new(format!("revoke-{index}"))
                                .label("接続を解除")
                                .disabled(disabled)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.dispatch(
                                        Intent::RevokeDevice(op::RevokeDevice { id: node.clone() }),
                                        Action::Revoke,
                                    );
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        body = body
            .child("別の Host に接続")
            .child(Textarea::new(&self.pairing).aria_label("ペアリング招待"))
            .child(
                Button::new("pair")
                    .label("ペアリング")
                    .disabled(disabled)
                    .on_click(cx.listener(|view, _, _, cx| {
                        match serde_json::from_str::<Invitation>(
                            view.pairing.read(cx).value().as_ref(),
                        ) {
                            Ok(invitation) => view.dispatch(
                                Intent::PairRemoteHost(op::PairRemoteHost {
                                    invitation,
                                    name: "Bex Desktop".into(),
                                }),
                                Action::Pair,
                            ),
                            Err(error) => view.error = Some(error.to_string()),
                        }
                        cx.notify();
                    })),
            );
        for host in &self.snapshot.management.remotes {
            let id = host.id.clone();
            body = body.child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().child(host.name.clone()))
                    .child(
                        Button::new(format!("remove-{id}"))
                            .label("保存した接続を削除")
                            .disabled(disabled)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.dispatch(
                                    Intent::RemoveRemoteHost(op::RemoveRemoteHost {
                                        id: id.clone(),
                                    }),
                                    Action::Remove(id.clone()),
                                );
                                cx.notify();
                            })),
                    ),
            );
        }
        body.child(
            Button::new("refresh-hosts")
                .label("接続一覧を更新")
                .disabled(disabled)
                .on_click(cx.listener(|view, _, _, cx| {
                    view.dispatch(
                        Intent::LoadHostManagement(op::LoadHostManagement),
                        Action::Refresh,
                    );
                    cx.notify();
                })),
        )
        .into_any_element()
    }
}
