//! The Device surface: Host-owned discovery, setup and live device frames.
mod device_decoder;

use super::PanelTab;
use crate::app::{Desktop, ui::{color, icon, tint}};
use agent_core::state::{
    DeviceActionIntent, DeviceDuoCommandIntent, DeviceDuoPoseIntent, Intent,
};
use gpui_kit::{component::{Sizable, button::{Button, ButtonVariants}, h_flex, input::{Input, InputState}, v_flex}, prelude::FluentBuilder, *};
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};

pub(super) struct DeviceState {
    thread: Option<String>,
    loaded: bool,
    subscribed: bool,
    detail_requests: BTreeSet<String>,
    accessibility_requests: BTreeSet<String>,
    event_log_requests: BTreeSet<String>,
    frames: BTreeMap<(String, String, String, u8), (u64, Arc<Image>)>,
    decoder: device_decoder::DeviceVideoDecoder,
    ssh_label: Entity<InputState>,
    ssh_target: Entity<InputState>,
    ssh_identity_file: Entity<InputState>,
    ssh_port: Entity<InputState>,
}

impl DeviceState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        Self {
            thread: None,
            loaded: false,
            subscribed: false,
            detail_requests: BTreeSet::new(),
            accessibility_requests: BTreeSet::new(),
            event_log_requests: BTreeSet::new(),
            frames: BTreeMap::new(),
            decoder: device_decoder::DeviceVideoDecoder::default(),
            ssh_label: cx.new(|cx| InputState::new(window, cx).placeholder("Build server")),
            ssh_target: cx.new(|cx| InputState::new(window, cx).placeholder("user@host")),
            ssh_identity_file: cx.new(|cx| InputState::new(window, cx).placeholder("~/.ssh/id_ed25519")),
            ssh_port: cx.new(|cx| InputState::new(window, cx).placeholder("22")),
        }
    }

    pub(super) fn reset(&mut self) {
        self.thread = None;
        self.loaded = false;
        self.subscribed = false;
        self.detail_requests.clear();
        self.accessibility_requests.clear();
        self.event_log_requests.clear();
        self.frames.clear();
        self.decoder.reset();
    }
}

impl Desktop {
    fn configure_device_ssh_host(&mut self, cx: &mut Context<Desktop>) {
        let label = self.panels.device.ssh_label.read(cx).value().trim().to_owned();
        let target = self.panels.device.ssh_target.read(cx).value().trim().to_owned();
        if label.is_empty() || target.is_empty() {
            return;
        }
        let identity_file = self.panels.device.ssh_identity_file.read(cx).value().trim().to_owned();
        let port = self
            .panels
            .device
            .ssh_port
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0);
        let mut id = format!("ssh-{}", target.chars().filter(|character| character.is_ascii_alphanumeric() || *character == '-' || *character == '_').collect::<String>());
        id.truncate(120);
        if id == "ssh-" {
            id = "ssh-host".into();
        }
        let view = self.snapshot.device();
        let mut hosts = view
            .hosts
            .iter()
            .filter_map(|host| host.target.as_ref().map(|target| agent_core::state::DeviceHostInput {
                id: host.id.clone(),
                label: host.label.clone(),
                target: target.clone(),
                identity_file: host.identity_file.clone(),
                port: host.port,
            }))
            .collect::<Vec<_>>();
        if let Some(existing) = hosts.iter_mut().find(|host| host.target == target) {
            existing.id = id;
            existing.label = label;
            existing.identity_file = (!identity_file.is_empty()).then_some(identity_file);
            existing.port = port;
        } else {
            hosts.push(agent_core::state::DeviceHostInput {
                id,
                label,
                target,
                identity_file: (!identity_file.is_empty()).then_some(identity_file),
                port,
            });
        }
        self.perform(Intent::UpdateDeviceHosts { hosts });
    }

    fn remove_device_ssh_host(&mut self, id: &str) {
        let hosts = self
            .snapshot
            .device()
            .hosts
            .into_iter()
            .filter_map(|host| {
                (host.target.is_some() && host.id != id).then(|| agent_core::state::DeviceHostInput {
                    id: host.id,
                    label: host.label,
                    target: host.target.unwrap_or_default(),
                    identity_file: host.identity_file,
                    port: host.port,
                })
            })
            .collect();
        self.perform(Intent::UpdateDeviceHosts { hosts });
    }

    fn attach_device_recording(&self, draft_key: String, recording: agent_core::view::device::DeviceRecordingView) {
        let file_name = recording.file_name.clone();
        if file_name.trim().is_empty() {
            let message = "The Host did not return a playable device recording. Save or attach is unavailable until recording finalization succeeds.".to_owned();
            self.stage(draft_key, move || {
                Err::<(Vec<agent_core::state::LocalFile>, Option<String>), String>(message)
            });
            return;
        }
        let mime_type = recording.mime_type.clone();
        if mime_type.trim().is_empty() {
            let message = "The Host did not return a playable device recording. Save or attach is unavailable until recording finalization succeeds.".to_owned();
            self.stage(draft_key, move || {
                Err::<(Vec<agent_core::state::LocalFile>, Option<String>), String>(message)
            });
            return;
        }
        if recording.bytes.is_empty() {
            let message = "The Host did not return a playable device recording. Save or attach is unavailable until recording finalization succeeds.".to_owned();
            self.stage(draft_key, move || {
                Err::<(Vec<agent_core::state::LocalFile>, Option<String>), String>(message)
            });
            return;
        }
        let name = file_name;
        let bytes = recording.bytes;
        let directory = self.attachments.directory.clone();
        self.stage(draft_key, move || {
            let path = directory
                .path()
                .join(format!("{}-{}", uuid::Uuid::new_v4(), name));
            std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
            Ok((
                vec![agent_core::state::LocalFile {
                    path: path.to_string_lossy().into_owned(),
                    name,
                    mime_type,
                }],
                None,
            ))
        });
    }

    pub(super) fn sync_device(&mut self, cx: &mut Context<Desktop>) {
        if !self.panel_shows(PanelTab::Device) {
            return;
        }
        let Some(thread) = self.thread_id() else { return };
        if self.panels.device.thread.as_deref() != Some(thread.as_str()) {
            self.panels.device.thread = Some(thread.clone());
            self.panels.device.loaded = false;
            self.panels.device.subscribed = false;
            self.panels.device.detail_requests.clear();
            self.panels.device.accessibility_requests.clear();
            self.panels.device.event_log_requests.clear();
            self.panels.device.frames.clear();
            self.panels.device.decoder.reset();
        }
        if !self.panels.device.loaded {
            self.panels.device.loaded = true;
            self.perform(Intent::LoadDevices);
        }
        if !self.panels.device.subscribed {
            self.panels.device.subscribed = true;
            self.perform(Intent::SubscribeDevice);
        }
        for session in self.snapshot.device().sessions.iter().filter(|session| session.thread_id == thread) {
            let key = format!("{}:{}", session.host_id, session.device_id);
            if self.panels.device.detail_requests.insert(key) {
                self.perform(Intent::LoadDeviceDetail {
                    host_id: Some(session.host_id.clone()),
                    device_id: session.device_id.clone(),
                });
            }
            let accessibility_key = format!("{}:{}", session.host_id, session.device_id);
            if self.panels.device.accessibility_requests.insert(accessibility_key) {
                self.perform(Intent::LoadDeviceAccessibility {
                    host_id: Some(session.host_id.clone()),
                    device_id: session.device_id.clone(),
                });
            }
            let log_key = format!("{}:{}", session.host_id, session.device_id);
            if self.panels.device.event_log_requests.insert(log_key) {
                self.perform(Intent::LoadDeviceEventLog {
                    host_id: Some(session.host_id.clone()),
                    device_id: session.device_id.clone(),
                    limit: 100,
                });
            }
        }
        let view = self.snapshot.device();
        let active_sessions = view
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread)
            .map(|session| {
                (
                    session.thread_id.clone(),
                    session.host_id.clone(),
                    session.device_id.clone(),
                    session.session_epoch.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        self.panels.device.decoder.retain_sessions(&active_sessions);
        self.panels.device.frames.retain(|(host_id, device_id, epoch, _), _| {
            active_sessions.iter().any(|(_, active_host, active_device, active_epoch)| {
                active_host == host_id && active_device == device_id && active_epoch == epoch
            })
        });
        let events = view
            .video_events
            .iter()
            .filter(|frame| frame.thread_id == thread)
            .cloned()
            .collect::<Vec<_>>();
        for image in self.panels.device.decoder.push(&events) {
            let format = match image.format {
                device_decoder::DeviceImageFormat::Jpeg => ImageFormat::Jpeg,
            };
            self.panels.device.frames.insert(
                (
                    image.host_id.clone(),
                    image.device_id.clone(),
                    image.session_epoch.clone(),
                    image.screen_id,
                ),
                (
                    image.sequence,
                    Arc::new(Image::from_bytes(format, image.bytes)),
                ),
            );
            cx.notify();
        }
    }

    pub(super) fn render_device(&mut self, cx: &mut Context<Desktop>) -> AnyElement {
        let view = self.snapshot.device();
        let frame_images = self
            .panels
            .device
            .frames
            .values()
            .map(|((host_id, device_id, _, _), (_, image))| {
                (host_id.clone(), device_id.clone(), image.clone())
            })
            .collect::<Vec<_>>();
        let enabled = view.enabled;
        let owner = cx.entity().downgrade();
        let current_thread = self.thread_id();
        let action_targets = view
            .sessions
            .iter()
            .filter(|session| current_thread.as_deref() == Some(session.thread_id.as_str()))
            .map(|session| (session.host_id.clone(), session.device_id.clone(), session.platform.clone()))
            .collect::<Vec<_>>();
        let current_thread_has_session = !action_targets.is_empty();
        let entries = view.devices.iter().map(|device| {
        let host_id = device.host_id.clone();
        let device_id = device.id.clone();
        let platform = device.platform.clone();
        let opened = view.sessions.iter().any(|session| {
            current_thread.as_deref() == Some(session.thread_id.as_str())
                && session.host_id == host_id
                && session.device_id == device_id
        });
        h_flex()
            .id(SharedString::from(format!("device-{}-{}", host_id, device_id)))
            .w_full()
            .h_8()
            .gap_2()
            .px_2()
            .rounded_md()
            .hover(|row| row.bg(tint("accentSurface", 0.6)))
            .child(icon(if device.booted { "circle-play" } else { "smartphone" }).size_4())
            .child(v_flex().flex_1().min_w_0().child(div().truncate().child(format!("{} · {}", device.name, device.platform))))
            .child(Button::new(SharedString::from(format!("open-{host_id}-{device_id}"))).label(if opened { "Close" } else { "Open" }).small().on_click({
                let owner = owner.clone();
                move |_, _, cx| {
                    let _ = owner.update(cx, |view, _| {
                        if opened {
                            view.perform(Intent::CloseDevice { host_id: Some(host_id.clone()), device_id: Some(device_id.clone()), shutdown: false });
                        } else {
                            view.perform(Intent::OpenDevice {
                                host_id: Some(host_id.clone()),
                                device_id: device_id.clone(),
                                platform: platform.clone(),
                                boot: true,
                            });
                        }
                    });
                }
            }))
        });
        let hosts = view.hosts.iter().map(|host| {
            let retry_id = host.id.clone();
            let detail = if host.unavailable_reasons.is_empty() {
                format!("{} · {}", host.kind, host.platforms.join(", "))
            } else {
                host.unavailable_reasons.join(" ")
            };
            h_flex()
                .w_full()
                .gap_1()
                .child(div().flex_1().text_2xs().text_color(color("textMuted")).child(format!("{}: {}", host.label, detail)))
                .child(Button::new(SharedString::from(format!("retry-device-host-{}", host.id))).label("Retry").xsmall().on_click(cx.listener(move |view, _, _, _| view.perform(Intent::RetryDeviceHost { host_id: retry_id.clone() }))))
                .when(host.target.is_some(), |row| {
                    let id = host.id.clone();
                    row.child(Button::new(SharedString::from(format!("remove-device-host-{id}"))).label("Remove").xsmall().on_click(cx.listener(move |view, _, _, _| view.remove_device_ssh_host(&id))))
                })
        });
        let device_controls = action_targets.into_iter().map(|(host_id, device_id, platform)| {
            let target = format!("{host_id}:{device_id}");
            h_flex()
                .id(SharedString::from(format!("device-controls-{target}")))
                .gap_1()
                .child(div().text_2xs().text_color(color("textMuted")).child(target))
                .child(Button::new(SharedString::from(format!("device-dark-{host_id}-{device_id}"))).label("Dark appearance").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::SetAppearance { dark: true },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-text-large-{host_id}-{device_id}"))).label("Larger text").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::SetTextSize { size: "large".into() },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-motion-{host_id}-{device_id}"))).label("Reduce motion").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::SetToggle { setting: "reduceMotion".into(), value: true },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-light-{host_id}-{device_id}"))).label("Light appearance").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::SetAppearance { dark: false },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-home-{host_id}-{device_id}"))).label("Home").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::HardwareButton { button: "home".into() },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-back-{host_id}-{device_id}"))).label("Back").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::HardwareButton { button: "back".into() },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-recents-{host_id}-{device_id}"))).label("Recents").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::HardwareButton { button: "recents".into() },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-power-{host_id}-{device_id}"))).label("Power").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::HardwareButton { button: "power".into() },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-enter-{host_id}-{device_id}"))).label("Enter").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| {
                        view.perform(Intent::DeviceAction {
                            host_id: Some(host_id.clone()),
                            device_id: device_id.clone(),
                            action: DeviceActionIntent::Key {
                                code: "Enter".into(),
                                key: "Enter".into(),
                                down: true,
                                meta: false,
                                ctrl: false,
                            },
                        });
                        view.perform(Intent::DeviceAction {
                            host_id: Some(host_id.clone()),
                            device_id: device_id.clone(),
                            action: DeviceActionIntent::Key {
                                code: "Enter".into(),
                                key: "Enter".into(),
                                down: false,
                                meta: false,
                                ctrl: false,
                            },
                        });
                    }
                })))
                .child(Button::new(SharedString::from(format!("device-touch-center-{host_id}-{device_id}"))).label("Touch center").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| {
                        view.perform(Intent::DeviceAction {
                            host_id: Some(host_id.clone()),
                            device_id: device_id.clone(),
                            action: DeviceActionIntent::Touch { phase: "begin".into(), x: 0.5, y: 0.5, raw: false },
                        });
                        view.perform(Intent::DeviceAction {
                            host_id: Some(host_id.clone()),
                            device_id: device_id.clone(),
                            action: DeviceActionIntent::Touch { phase: "end".into(), x: 0.5, y: 0.5, raw: false },
                        });
                    }
                })))
                .child(Button::new(SharedString::from(format!("device-rotate-{host_id}-{device_id}"))).label("Rotate").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::Rotate,
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-screenshot-{host_id}-{device_id}"))).label("Screenshot").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::CaptureDeviceScreenshot {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                    })
                })))
                .when(platform == "ios", |panel| panel
                .child(Button::new(SharedString::from(format!("device-fold-book-{host_id}-{device_id}"))).label("Book pose").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::Duo {
                            command: DeviceDuoCommandIntent::Pose { value: DeviceDuoPoseIntent::Book },
                        },
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-duo-table-{host_id}-{device_id}"))).label("Table mode").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::DeviceAction {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        action: DeviceActionIntent::Duo {
                            command: DeviceDuoCommandIntent::Table { value: true },
                        },
                    })
                })))
                )
                .child(Button::new(SharedString::from(format!("device-record-{host_id}-{device_id}"))).label("Record").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::StartDeviceRecording {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                        format: "mp4".into(),
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-stop-record-{host_id}-{device_id}"))).label("Stop record").xsmall().on_click(cx.listener({
                    let host_id = host_id.clone();
                    let device_id = device_id.clone();
                    move |view, _, _, _| view.perform(Intent::StopDeviceRecording {
                        host_id: Some(host_id.clone()),
                        device_id: device_id.clone(),
                    })
                })))
                .child(Button::new(SharedString::from(format!("device-power-off-{host_id}-{device_id}"))).label("Power off").xsmall().on_click(cx.listener({
                    move |view, _, _, _| view.perform(Intent::CloseDevice {
                        host_id: Some(host_id.clone()),
                        device_id: Some(device_id.clone()),
                        shutdown: true,
                    })
                })))
        });
        v_flex()
        .id("device-panel")
        .flex_1()
        .min_h_0()
        .gap_2()
        .p_3()
        .overflow_y_scroll()
        .child(h_flex().justify_between().child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Device" )).child(Button::new("device-refresh").label("Refresh").small().on_click(cx.listener(|view, _, _, _| view.perform(Intent::LoadDevices)))))
        .when(!enabled, |panel| {
            panel.child(v_flex().gap_2().child(div().text_sm().child("Device support is off.")).child(Button::new("device-enable").label("Enable device support").small().on_click(cx.listener(|view, _, _, _| {
                view.perform(Intent::ConfigureDevices {
                    enabled: Some(true),
                    agent_access_enabled: None,
                    onboarding_completed: Some(false),
                });
            })))
        })
        .when(enabled, |panel| {
            panel
                .child(div().text_2xs().text_color(color("textMuted")).child(format!("Host status: {}", view.status)))
                .child(h_flex().gap_1()
                    .child(Button::new("device-inspect-tools").label("Inspect tools").xsmall().on_click(cx.listener(|view, _, _, _| view.perform(Intent::InspectDevices { host_id: None }))))
                    .child(Button::new("device-update-hub").label("Update hub").xsmall().on_click(cx.listener(|view, _, _, _| view.perform(Intent::UpdateDeviceTool { host_id: None, tool: "hub".into() }))))
                    .child(Button::new("device-update-agent").label("Update agent").xsmall().on_click(cx.listener(|view, _, _, _| view.perform(Intent::UpdateDeviceTool { host_id: None, tool: "agent".into() })))))
                .when_some(view.status_detail.clone(), |panel, detail| {
                    panel.child(div().text_2xs().text_color(color("textMuted")).child(detail))
                })
                .child(v_flex().gap_0p5().children(hosts))
                .child(v_flex().gap_1()
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("SSH device host"))
                    .child(h_flex().gap_1()
                        .child(Input::new(&self.panels.device.ssh_label).small().flex_1().aria_label("SSH host label"))
                        .child(Input::new(&self.panels.device.ssh_target).small().flex_1().aria_label("SSH host target")))
                    .child(h_flex().gap_1()
                        .child(Input::new(&self.panels.device.ssh_identity_file).small().flex_1().aria_label("SSH identity file"))
                        .child(Input::new(&self.panels.device.ssh_port).small().w(px(64.)).aria_label("SSH port"))
                        .child(Button::new("device-add-ssh").label("Save host").small().on_click(cx.listener(|view, _, _, cx| view.configure_device_ssh_host(cx)))))
                .when(!view.agent_access_enabled, |panel| {
                    panel.child(Button::new("device-agent-enable").label("Enable agent device access").small().on_click(cx.listener(|view, _, _, _| {
                        view.perform(Intent::ConfigureDevices {
                            enabled: None,
                            agent_access_enabled: Some(true),
                            onboarding_completed: Some(true),
                        });
                    })))
                })
                .child(v_flex().gap_0p5().children(entries))
                .when(current_thread_has_session, |panel| {
                    panel.child(v_flex().gap_0p5().children(device_controls))
                })
                .child(v_flex().gap_0p5().children(view.foreground.iter().filter(|foreground| {
                    current_thread.as_deref().is_some_and(|thread| {
                        view.sessions.iter().any(|session| {
                            session.thread_id == thread
                                && session.host_id == foreground.host_id
                                && session.device_id == foreground.device_id
                        })
                    })
                }).map(|foreground| {
                    div().text_2xs().text_color(color("textMuted")).child(format!(
                        "Foreground {}:{} · {}",
                        foreground.host_id,
                        foreground.device_id,
                        foreground.app_id.as_deref().unwrap_or("unknown")
                    ))
                })))
                .child(v_flex().gap_0p5().children(view.screens.iter().filter(|screen| {
                    current_thread.as_deref().is_some_and(|thread| {
                        screen.thread_id.as_deref() == Some(thread)
                    })
                }).map(|screen| {
                    div().text_2xs().text_color(color("textMuted")).child(format!(
                        "Screen {} · {}×{} · {}{}",
                        screen.screen_id.map_or_else(|| "main".into(), |id| id.to_string()),
                        screen.width,
                        screen.height,
                        screen.orientation,
                        screen.hinge_pose.as_deref().map_or(String::new(), |pose| format!(" · {pose}"))
                    ))
                })))
                .when(!view.accessibility.is_empty(), |panel| {
                    panel.child(v_flex().gap_0p5()
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Accessibility overlay"))
                        .children(view.accessibility.iter().flat_map(|tree| tree.elements.iter().filter(|element| !element.label.is_empty()).take(20).map(|element| div().text_2xs().text_color(color("textMuted")).child(format!("{} · {}", element.role, element.label)))))
                    )
                })
                .when(!view.event_log.is_empty(), |panel| {
                    panel.child(v_flex().gap_0p5()
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).child("Device event log"))
                        .children(view.event_log.iter().rev().take(20).map(|entry| div().text_2xs().text_color(color("textMuted")).child(format!("{} · {}", entry.kind, entry.summary)))))
                })
                .when_some(view.last_recording.clone().filter(|recording| current_thread.as_deref() == Some(recording.thread_id.as_str())), |panel, recording| {
                    let recording_for_attach = recording.clone();
                    let draft_key = self.snapshot.draft_key();
                    panel.child(h_flex().gap_1()
                        .child(div().text_2xs().text_color(color("textMuted")).child(format!("Recording ready · {} frames · {} bytes", recording.frame_count, recording.byte_count)))
                        .child(Button::new("device-attach-recording").label("Attach recording").xsmall().on_click(cx.listener(move |view, _, _, _| view.attach_device_recording(draft_key.clone(), recording_for_attach.clone()))))
                    )
                })
                .when_some(self.panels.device.decoder.error().map(str::to_owned), |panel, error| {
                    panel.child(div().text_2xs().text_color(color("textMuted")).child(error))
                })
                .child(if frame_images.is_empty() {
                    div().flex_1().min_h_0().items_center().justify_center().text_color(color("textMuted")).child("Open a device to see its live frame").into_any_element()
                } else {
                    h_flex().flex_1().min_h_0().items_center().justify_center().gap_2().children(frame_images.into_iter().map(|(host_id, device_id, image)| {
                        let accessibility = view.accessibility.iter()
                            .filter(|tree| tree.host_id == host_id && tree.device_id == device_id)
                            .flat_map(|tree| tree.elements.iter())
                            .filter(|element| !element.label.is_empty())
                            .map(|element| {
                                div()
                                    .absolute()
                                    .left(relative(element.x))
                                    .top(relative(element.y))
                                    .w(relative(element.width))
                                    .h(relative(element.height))
                                    .border_1()
                                    .border_color(tint("accent", 0.9))
                                    .aria_label(element.label.clone())
                            });
                        div().relative().flex_1().min_h_0().flex().items_center().justify_center()
                            .child(img(image).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                            .children(accessibility)
                    })).into_any_element()
                })
        })
        .into_any_element()
}
