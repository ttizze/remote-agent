//! Connections: this machine's Host (pairing other clients, the clients it
//! trusts) and the environments this client connects to.
use super::ui::{color, icon, tint};
use crate::{Runtime, store_session::StoreSession};
use agent_core::{
    connection::{Outcome, StoreOptions},
    state::{Intent, Snapshot},
};
use agent_protocol::models::{Invitation, RemoteHost};
use gpui_kit::{
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        input::{Input, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        *,
    },
    prelude::FluentBuilder,
    *,
};
use std::{io::Write, sync::Arc};

pub(crate) enum HostEvent {
    Selected(Option<RemoteHost>),
    Removed(String),
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
pub(crate) struct Hosts {
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<Update>,
    pairing: Entity<TextareaState>,
    ssh: Entity<InputState>,
    use_ssh: bool,
    invitation: Option<(String, tempfile::NamedTempFile)>,
    busy: bool,
    connecting: bool,
    error: Option<String>,
    /// The environment the window shows, `None` for this machine.
    current: Option<String>,
    current_connected: bool,
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
                    .placeholder("Paste the pairing code from the other environment")
                    .auto_grow(3, 6)
            }),
            ssh: cx.new(|cx| InputState::new(window, cx).placeholder("user@your-server")),
            use_ssh: true,
            invitation: None,
            busy: false,
            connecting: false,
            error: None,
            current: None,
            current_connected: false,
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
                connections
                    .connect(None, Snapshot::default(), StoreOptions::default())
                    .await,
                runtime,
                updates,
                Update::Connected,
                |_| Update::Snapshot,
                None,
            )
            .await;
        });
    }
    pub(super) fn refresh(&mut self) {
        if self.busy || self.connecting {
            return;
        }
        if self.snapshot.connected {
            self.dispatch(Intent::LoadHostManagement, Action::None);
        } else {
            self.connect();
        }
    }
    /// Follows the environment the window is connected to.
    pub(super) fn set_current(
        &mut self,
        current: Option<String>,
        connected: bool,
        cx: &mut Context<Self>,
    ) {
        if self.current != current || self.current_connected != connected {
            self.current = current;
            self.current_connected = connected;
            cx.notify();
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
            let result = receipt
                .await
                .map_err(|error| error.to_string())
                .and_then(|r| r.map_err(|error| error.to_string()));
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
                        self.dispatch(Intent::LoadHostManagement, Action::None);
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
                            if let Some(invitation) = &self.snapshot.invitation {
                                match invitation_qr(invitation) {
                                    Ok(invitation) => self.invitation = Some(invitation),
                                    Err(error) => self.error = Some(error),
                                }
                            }
                        }
                        Action::Pair => {
                            self.pairing
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            window.close_dialog(cx);
                            if let Outcome::RemoteHostPaired { id } = outcome
                                && let Some(host) =
                                    self.snapshot.remote_hosts.iter().find(|host| host.id == id)
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
            Intent::PairRemoteHost {
                invitation,
                name: "Bex Desktop".into(),
            },
            Action::Pair,
        );
    }
    fn submit_pairing(&mut self, cx: &mut Context<Self>) {
        if self.use_ssh {
            self.busy = true;
            self.error = None;
            let destination = self.ssh.read(cx).value().to_string();
            let updates = self.updates.clone();
            self.runtime.handle.spawn(async move {
                let result = crate::platform::ssh_invitation(&destination).await;
                let _ = updates.send(Update::SshInvitation(result)).await;
            });
        } else {
            match serde_json::from_str::<Invitation>(self.pairing.read(cx).value().as_ref()) {
                Ok(invitation) => self.pair(invitation),
                Err(_) => {
                    self.error =
                        Some("Paste the pairing code the other environment created.".into())
                }
            }
        }
        cx.notify();
    }

    /// The composer's environment picker: this machine and saved environments.
    pub(super) fn menu(
        owner: &Entity<Self>,
        id: &'static str,
        current: Option<&str>,
        fallback_name: &str,
        disabled: bool,
        cx: &App,
    ) -> impl IntoElement {
        let view = owner.read(cx);
        let hosts = &view.snapshot.remote_hosts;
        let name: SharedString = current
            .and_then(|id| hosts.iter().find(|host| host.id == id))
            .map_or_else(
                || fallback_name.to_owned().into(),
                |host| host.name.clone().into(),
            );
        let current = current.map(str::to_owned);
        let hosts = hosts.clone();
        let owner = owner.downgrade();
        Button::new(id)
            .debug_selector(move || id.into())
            .disabled(disabled)
            .accessibility_label(format!("Environment: {name}"))
            .tooltip(format!("Environment: {name}"))
            .label(name)
            .icon(icon("monitor"))
            .dropdown_caret(true)
            .h_8()
            .min_w_0()
            .max_w(px(200.))
            .flex_shrink_1()
            .ghost()
            .dropdown_menu(move |mut menu, _, _| {
                let local = owner.clone();
                menu = menu.item(
                    PopupMenuItem::new("This machine")
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

    fn open_add_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let hosts = cx.entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            dialog
                .title("Add Environment")
                .w(px(560.))
                .child(hosts.update(cx, |hosts, cx| hosts.render_add_environment(window, cx)))
        });
    }

    fn render_add_environment(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.busy || !self.snapshot.connected;
        let mode = |id: &'static str,
                    title: &'static str,
                    description: &'static str,
                    icon_name: &'static str,
                    ssh: bool,
                    selected: bool,
                    cx: &mut Context<Self>| {
            v_flex()
                .id(id)
                .flex_1()
                .gap_1()
                .p_3()
                .rounded(px(10.))
                .border_1()
                .cursor_pointer()
                .border_color(if selected {
                    tint("messageAction", 0.7)
                } else {
                    tint("border", 0.7)
                })
                .when(selected, |card| card.bg(tint("messageAction", 0.08)))
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .font_medium()
                        .child(icon(icon_name).size_4())
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(description),
                )
                .on_click(cx.listener(move |hosts, _, _, cx| {
                    hosts.use_ssh = ssh;
                    hosts.error = None;
                    cx.notify();
                }))
        };
        let fields =
            if self.use_ssh {
                v_flex()
                    .gap(px(6.))
                    .child(div().text_xs().font_medium().child("SSH host"))
                    .child(
                        Input::new(&self.ssh)
                            .aria_label("SSH host")
                            .disabled(disabled),
                    )
                    .child(div().text_xs().text_color(color("textMuted")).child(
                        "Connects with your SSH keys to a server where the Host is running.",
                    ))
            } else {
                v_flex()
                    .gap(px(6.))
                    .child(div().text_xs().font_medium().child("Pairing code"))
                    .child(
                        Textarea::new(&self.pairing)
                            .aria_label("Pairing code")
                            .readonly(disabled),
                    )
            };
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("Pair another environment to this client."),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(mode(
                        "pair-remote",
                        "Remote link",
                        "Paste a pairing code from another environment.",
                        "chevrons-left-right-ellipsis",
                        false,
                        !self.use_ssh,
                        cx,
                    ))
                    .child(mode(
                        "pair-ssh",
                        "SSH",
                        "Use local SSH config, agent, and tunnels for the backend.",
                        "terminal",
                        true,
                        self.use_ssh,
                        cx,
                    )),
            )
            .child(fields)
            .when_some(self.error.clone(), |form, error| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(color("errorForeground"))
                        .child(agent_core::presentation::error::error_message(&error)),
                )
            })
            .child(
                h_flex().justify_end().child(
                    Button::new("pair-submit")
                        .primary()
                        .label(if self.busy {
                            "Adding…"
                        } else {
                            "Add environment"
                        })
                        .disabled(disabled)
                        .on_click(cx.listener(|hosts, _, _, cx| hosts.submit_pairing(cx))),
                ),
            )
            .into_any_element()
    }
}

/// The invitation as text and as a QR code image file.
fn invitation_qr(invitation: &Invitation) -> Result<(String, tempfile::NamedTempFile), String> {
    let text = serde_json::to_string(invitation).map_err(|error| error.to_string())?;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
        .map_err(|error| error.to_string())?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .build();
    let mut file = tempfile::Builder::new()
        .prefix("pairing-")
        .suffix(".svg")
        .tempfile()
        .map_err(|error| error.to_string())?;
    file.write_all(svg.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok((text, file))
}

fn section_title(
    title: impl Into<SharedString>,
    leading: Option<Icon>,
    action: Option<AnyElement>,
) -> Div {
    h_flex()
        .min_h_7()
        .px_4()
        .gap_4()
        .justify_between()
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .text_sm()
                .text_color(tint("text", 0.7))
                .children(leading.map(|leading| leading.size_4()))
                .child(div().truncate().child(title.into())),
        )
        .child(h_flex().min_h_7().justify_end().children(action))
}

fn card(rows: Vec<AnyElement>) -> Div {
    v_flex()
        .rounded(px(14.))
        .border_1()
        .border_color(tint("border", 0.6))
        .bg(tint("surface", 0.4))
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            div()
                .when(index > 0, |line| {
                    line.border_t_1().border_color(tint("border", 0.5))
                })
                .child(row)
        }))
}

fn row(
    title: impl IntoElement,
    description: Option<AnyElement>,
    control: Option<AnyElement>,
) -> Div {
    h_flex()
        .px_4()
        .py_3()
        .gap_8()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_medium().child(title))
                .children(description.map(|description| {
                    div()
                        .text_xs()
                        .text_color(tint("textMuted", 0.8))
                        .child(description)
                })),
        )
        .children(control.map(|control| h_flex().flex_shrink_0().gap_2().child(control)))
}

impl Render for Hosts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = !self.snapshot.connected || self.busy;
        let mut body = v_flex().w_full().gap_8();
        if let Some(error) = self.error.as_ref().or(self.snapshot.error.as_ref()) {
            body = body.child(
                div()
                    .px_4()
                    .text_sm()
                    .text_color(color("errorForeground"))
                    .child(agent_core::presentation::error::error_message(error)),
            );
        }
        if !self.snapshot.connected {
            body = body.child(
                v_flex()
                    .gap(px(10.))
                    .child(section_title("This machine", Some(icon("monitor")), None))
                    .child(card(vec![
                        row(
                            "Host",
                            Some(
                                div()
                                    .child("This machine's Host is not reachable.")
                                    .into_any_element(),
                            ),
                            Some(
                                Button::new("connect-host")
                                    .outline()
                                    .small()
                                    .label(if self.connecting {
                                        "Connecting…"
                                    } else {
                                        "Connect"
                                    })
                                    .disabled(self.connecting)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.connect();
                                        cx.notify();
                                    }))
                                    .into_any_element(),
                            ),
                        )
                        .into_any_element(),
                    ])),
            );
            return body.into_any_element();
        }
        let machine = self
            .snapshot
            .host_status
            .as_ref()
            .map(|status| status.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "This machine".into());
        let mut pairing = row(
            "Pairing link",
            Some(
                div()
                    .child("Pair a phone or another client with this machine.")
                    .into_any_element(),
            ),
            Some(
                Button::new("invite")
                    .outline()
                    .small()
                    .icon(icon("qr-code"))
                    .label("Create link")
                    .disabled(disabled)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.dispatch(Intent::CreateInvitation, Action::Invite);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        )
        .into_any_element();
        if let Some((_, file)) = &self.invitation {
            pairing = v_flex()
                .child(pairing)
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_5()
                        .px_4()
                        .pb_4()
                        .child(
                            div()
                                .p_2()
                                .rounded(px(10.))
                                .bg(hsla(0., 0., 1., 1.))
                                .child(img(file.path().to_path_buf()).w(px(184.)).h(px(184.))),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(200.))
                                .gap_3()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .child("Pairing link — scan to open on another device"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(color("textMuted"))
                                        .child("Works once. Scan the QR code with the mobile app, or copy the pairing code and paste it into another client."),
                                )
                                .child(
                                    Button::new("copy-invite")
                                        .outline()
                                        .small()
                                        .icon(icon("copy"))
                                        .label("Copy pairing code")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            if let Some((text, _)) = &view.invitation {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text.clone(),
                                                ));
                                                window.push_notification(
                                                    notification::Notification::success(
                                                        "Paste it into another client to finish pairing.",
                                                    )
                                                    .title("Pairing code copied"),
                                                    cx,
                                                );
                                            }
                                        })),
                                ),
                        ),
                )
                .into_any_element();
        }
        body = body.child(
            v_flex()
                .gap(px(10.))
                .child(section_title(machine, Some(icon("monitor")), None))
                .child(card(vec![pairing])),
        );
        let mut clients = Vec::new();
        match &self.snapshot.host_status {
            None => clients.push(row("Loading…", None, None).into_any_element()),
            Some(status) if status.devices.is_empty() => clients.push(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(tint("textMuted", 0.6))
                    .child("No pairing links or client sessions.")
                    .into_any_element(),
            ),
            Some(status) => {
                for (index, node) in status.devices.iter().enumerate() {
                    let node = node.clone();
                    clients.push(
                        row(
                            div()
                                .truncate()
                                .font_family("monospace")
                                .child(node.clone()),
                            None,
                            Some(
                                Button::new(("revoke", index))
                                    .outline()
                                    .small()
                                    .label("Revoke")
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.dispatch(
                                            Intent::RevokeDevice { id: node.clone() },
                                            Action::None,
                                        );
                                        cx.notify();
                                    }))
                                    .into_any_element(),
                            ),
                        )
                        .into_any_element(),
                    );
                }
            }
        }
        body = body.child(
            v_flex()
                .gap(px(10.))
                .child(section_title(
                    "Authorized clients",
                    None,
                    Some(
                        Button::new("refresh-hosts")
                            .icon(icon("refresh-cw"))
                            .ghost()
                            .xsmall()
                            .text_color(color("textMuted"))
                            .accessibility_label("Refresh")
                            .tooltip("Refresh")
                            .disabled(disabled)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.refresh();
                                cx.notify();
                            }))
                            .into_any_element(),
                    ),
                ))
                .child(card(clients)),
        );
        let mut environments = vec![self.environment_row(None, cx)];
        for remote in self.snapshot.remote_hosts.clone() {
            environments.push(self.environment_row(Some(remote), cx));
        }
        if self.snapshot.remote_hosts.is_empty() {
            environments.push(
                row(
                    "No saved remote environments",
                    Some(
                        div()
                            .child("Click “Add environment” to pair another environment.")
                            .into_any_element(),
                    ),
                    None,
                )
                .into_any_element(),
            );
        }
        body.child(
            v_flex()
                .gap(px(10.))
                .child(section_title(
                    "Environments",
                    None,
                    Some(
                        Button::new("add-environment")
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .label("Add environment")
                            .text_color(color("textMuted"))
                            .disabled(disabled)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_add_environment(window, cx)
                            }))
                            .into_any_element(),
                    ),
                ))
                .child(card(environments)),
        )
        .into_any_element()
    }
}

impl Hosts {
    /// One environment this client can show: this machine or a saved one.
    fn environment_row(&self, remote: Option<RemoteHost>, cx: &mut Context<Self>) -> AnyElement {
        let id = remote.as_ref().map(|host| host.id.clone());
        let selected = self.current == id;
        let name = remote
            .as_ref()
            .map_or_else(|| "This machine".to_owned(), |host| host.name.clone());
        let status = if selected {
            if self.current_connected {
                "Connected"
            } else {
                "Connecting"
            }
        } else {
            "Not connected"
        };
        let key = id.clone().unwrap_or_else(|| "local".into());
        let destination = remote.clone();
        let mut controls = h_flex().gap(px(6.));
        if !selected {
            controls = controls.child(
                Button::new(SharedString::from(format!("connect-{key}")))
                    .outline()
                    .small()
                    .label("Connect")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(HostEvent::Selected(destination.clone()));
                    })),
            );
        }
        if let Some(remote_id) = id {
            let owner = cx.entity().downgrade();
            let disabled = self.busy || !self.snapshot.connected;
            controls = controls.child(
                Button::new(SharedString::from(format!("environment-menu-{key}")))
                    .icon(icon("ellipsis"))
                    .ghost()
                    .xsmall()
                    .text_color(color("textMuted"))
                    .accessibility_label(format!("More actions for {name}"))
                    .disabled(disabled)
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let owner = owner.clone();
                        let remote_id = remote_id.clone();
                        menu.item(PopupMenuItem::new("Remove from this device…").on_click(
                            move |_, _, cx| {
                                let id = remote_id.clone();
                                let _ = owner.update(cx, |view, cx| {
                                    view.dispatch(
                                        Intent::RemoveRemoteHost { id: id.clone() },
                                        Action::Remove(id),
                                    );
                                    cx.notify();
                                });
                            },
                        ))
                    }),
            );
        }
        h_flex()
            .px_4()
            .py_3()
            .gap_3()
            .child(
                icon(if remote.is_some() { "server" } else { "laptop" })
                    .size_4()
                    .text_color(color("textMuted")),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().truncate().text_sm().font_medium().child(name))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if selected && self.current_connected {
                                color("successForeground")
                            } else {
                                tint("textMuted", 0.8)
                            })
                            .child(status),
                    ),
            )
            .child(controls)
            .into_any_element()
    }
}
