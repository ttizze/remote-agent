//! The Device surface: Host-owned discovery, setup and live device frames.
mod device_decoder;

use super::PanelTab;
use crate::app::{
    Desktop,
    ui::{color, icon, tint},
};
use agent_core::state::{
    DeviceActionIntent, DeviceDuoCommandIntent, DeviceDuoOrientationIntent,
    DeviceDuoPhysicalIntent, DeviceDuoPoseIntent, DeviceFoldPostureIntent, Intent,
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

struct ActiveDeviceTouch {
    host_id: String,
    device_id: String,
    screen_id: u8,
    session_epoch: String,
    x: f32,
    y: f32,
}

pub(super) struct DeviceState {
    thread: Option<String>,
    loaded: bool,
    subscribed: bool,
    detail_requests: BTreeSet<String>,
    accessibility_requests: BTreeSet<String>,
    event_log_requests: BTreeSet<String>,
    frames: BTreeMap<(String, String, u8), (u64, Arc<Image>)>,
    frame_epochs: BTreeMap<(String, String, u8), String>,
    frame_bounds: BTreeMap<(String, String, u8), Bounds<Pixels>>,
    active_touch: Option<ActiveDeviceTouch>,
    keyboard_target: Option<(String, String, String)>,
    decoder: device_decoder::DeviceVideoDecoder,
    focus: FocusHandle,
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
            frame_epochs: BTreeMap::new(),
            frame_bounds: BTreeMap::new(),
            active_touch: None,
            keyboard_target: None,
            decoder: device_decoder::DeviceVideoDecoder::default(),
            focus: cx.focus_handle(),
            ssh_label: cx.new(|cx| InputState::new(window, cx).placeholder("Build server")),
            ssh_target: cx.new(|cx| InputState::new(window, cx).placeholder("user@host")),
            ssh_identity_file: cx
                .new(|cx| InputState::new(window, cx).placeholder("~/.ssh/id_ed25519")),
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
        self.frame_epochs.clear();
        self.frame_bounds.clear();
        self.active_touch = None;
        self.keyboard_target = None;
        self.decoder.reset();
    }
}

impl Desktop {
    fn release_device_input(&mut self) {
        let Some(thread) = self.thread_id() else { return };
        let sessions = self
            .snapshot
            .device()
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread)
            .map(|session| {
                (
                    session.host_id.clone(),
                    session.device_id.clone(),
                    session.session_epoch.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        for (host_id, device_id, session_epoch) in sessions {
            self.perform(Intent::ReleaseDeviceInput {
                host_id: Some(host_id),
                device_id,
                session_epoch: Some(session_epoch),
            });
        }
        self.panels.device.keyboard_target = None;
    }

    fn release_device_input_for(&mut self, host_id: &str, device_id: &str) {
        let session_epoch = self
            .snapshot
            .device()
            .sessions
            .iter()
            .find(|session| session.host_id == host_id && session.device_id == device_id)
            .map(|session| session.session_epoch.clone());
        self.perform(Intent::ReleaseDeviceInput {
            host_id: Some(host_id.to_owned()),
            device_id: device_id.to_owned(),
            session_epoch,
        });
        if self
            .panels
            .device
            .keyboard_target
            .as_ref()
            .is_some_and(|(current_host, current_device, _)| {
                current_host == host_id && current_device == device_id
            })
        {
            self.panels.device.keyboard_target = None;
        }
    }

    fn device_touch(
        &mut self,
        host_id: String,
        device_id: String,
        screen_id: u8,
        session_epoch: String,
        phase: &'static str,
        position: Point<Pixels>,
        frame_width: u32,
        frame_height: u32,
        cx: &mut App,
    ) {
        if phase == "begin" {
            self.send_device_touch_end();
        }
        let current_epoch = self
            .thread_id()
            .and_then(|thread| {
                self.snapshot
                    .device()
                    .sessions
                    .iter()
                    .find(|session| {
                        session.thread_id == thread
                            && session.host_id == host_id
                            && session.device_id == device_id
                    })
                    .map(|session| session.session_epoch.clone())
            });
        if current_epoch.as_deref() != Some(session_epoch.as_str()) {
            if self.panels.device.active_touch.as_ref().is_some_and(|touch| {
                touch.host_id == host_id
                    && touch.device_id == device_id
                    && touch.screen_id == screen_id
            }) {
                self.panels.device.active_touch = None;
            }
            cx.stop_propagation();
            return;
        }
        let key = (host_id.clone(), device_id.clone(), screen_id);
        let bounds = self
            .panels
            .device
            .frame_bounds
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let Some(point) = device_frame_point(bounds, position, frame_width, frame_height) else {
            if phase == "end" {
                self.send_device_touch_end();
            }
            cx.stop_propagation();
            return;
        };
        let (x, y) = point;
        self.perform(Intent::DeviceAction {
            host_id: Some(host_id.clone()),
            device_id: device_id.clone(),
            action: DeviceActionIntent::Touch {
                phase: phase.into(),
                x,
                y,
            },
        });
        if phase == "end" {
            self.panels.device.active_touch = None;
        } else {
            self.panels.device.active_touch = Some(ActiveDeviceTouch {
                host_id,
                device_id,
                screen_id,
                session_epoch,
                x,
                y,
            });
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn send_device_touch_end(&mut self) -> bool {
        let Some(touch) = self.panels.device.active_touch.take() else {
            return false;
        };
        let current_epoch = self.thread_id().and_then(|thread| {
            self.snapshot
                .device()
                .sessions
                .iter()
                .find(|session| {
                    session.thread_id == thread
                        && session.host_id == touch.host_id
                        && session.device_id == touch.device_id
                })
                .map(|session| session.session_epoch.clone())
        });
        if current_epoch.as_deref() != Some(touch.session_epoch.as_str()) {
            return false;
        }
        self.perform(Intent::DeviceAction {
            host_id: Some(touch.host_id),
            device_id: touch.device_id,
            action: DeviceActionIntent::Touch {
                phase: "end".into(),
                x: touch.x,
                y: touch.y,
            },
        });
        true
    }

    fn cancel_device_touch(&mut self, cx: &mut App) {
        if !self.send_device_touch_end() {
            return;
        }
        cx.notify();
    }

    fn device_key_down(
        &mut self,
        host_id: String,
        device_id: String,
        session_epoch: String,
        event: &KeyDownEvent,
        cx: &mut App,
    ) {
        self.send_device_key(host_id, device_id, session_epoch, &event.keystroke, true);
        cx.stop_propagation();
    }

    fn device_key_up(
        &mut self,
        host_id: String,
        device_id: String,
        session_epoch: String,
        event: &KeyUpEvent,
        cx: &mut App,
    ) {
        self.send_device_key(host_id, device_id, session_epoch, &event.keystroke, false);
        cx.stop_propagation();
    }

    fn send_device_key(
        &mut self,
        host_id: String,
        device_id: String,
        session_epoch: String,
        keystroke: &Keystroke,
        down: bool,
    ) {
        let facts = agent_core::state::canonical_device_key("", &keystroke.key);
        let thread_id = self.thread_id().unwrap_or_default();
        if !self.snapshot.device().sessions.iter().any(|session| {
            session.thread_id == thread_id
                && session.host_id == host_id
                && session.device_id == device_id
                && session.session_epoch == session_epoch
        }) {
            return;
        }
        let target = (host_id.clone(), device_id.clone(), session_epoch);
        if self.panels.device.keyboard_target.as_ref() != Some(&target) {
            if let Some((old_host, old_device, old_epoch)) =
                self.panels.device.keyboard_target.replace(target)
            {
                self.perform(Intent::ReleaseDeviceInput {
                    host_id: Some(old_host),
                    device_id: old_device,
                    session_epoch: Some(old_epoch),
                });
            }
        }
        self.perform(Intent::DeviceAction {
            host_id: Some(host_id),
            device_id,
            action: DeviceActionIntent::Key {
                code: facts.code,
                key: facts.key,
                down,
                meta: keystroke.modifiers.platform,
                ctrl: keystroke.modifiers.control,
                shift: keystroke.modifiers.shift,
                alt: keystroke.modifiers.alt,
            },
        });
    }

    fn configure_device_ssh_host(&mut self, cx: &mut Context<Desktop>) {
        let label = self
            .panels
            .device
            .ssh_label
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let target = self
            .panels
            .device
            .ssh_target
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if label.is_empty() || target.is_empty() {
            return;
        }
        let identity_file = self
            .panels
            .device
            .ssh_identity_file
            .read(cx)
            .value()
            .trim()
            .to_owned();
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
        let mut id = format!(
            "ssh-{}",
            target
                .chars()
                .filter(|character| character.is_ascii_alphanumeric()
                    || *character == '-'
                    || *character == '_')
                .collect::<String>()
        );
        id.truncate(120);
        if id == "ssh-" {
            id = "ssh-host".into();
        }
        let view = self.snapshot.device();
        let mut hosts = view
            .hosts
            .iter()
            .filter_map(|host| {
                host.target
                    .as_ref()
                    .map(|target| agent_core::state::DeviceHostInput {
                        id: host.id.clone(),
                        label: host.label.clone(),
                        target: target.clone(),
                        identity_file: host.identity_file.clone(),
                        port: host.port,
                    })
            })
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
                (host.target.is_some() && host.id != id).then(|| {
                    agent_core::state::DeviceHostInput {
                        id: host.id,
                        label: host.label,
                        target: host.target.unwrap_or_default(),
                        identity_file: host.identity_file,
                        port: host.port,
                    }
                })
            })
            .collect();
        self.perform(Intent::UpdateDeviceHosts { hosts });
    }

    fn attach_device_recording(
        &self,
        draft_key: String,
        recording: agent_core::view::device::DeviceRecordingView,
    ) {
        let Some((extension, mime_type)) = recording_file_type(&recording) else {
            let message = "The Host did not return a playable device recording. Save or attach is unavailable until recording finalization succeeds.".to_owned();
            self.stage(draft_key, move || {
                Err::<(Vec<agent_core::state::LocalFile>, Option<String>), String>(message)
            });
            return;
        };
        let name = format!("device-recording-{draft_key}.{extension}");
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
                    mime_type: mime_type.into(),
                }],
                None,
            ))
        });
    }

    pub(super) fn sync_device(&mut self, cx: &mut Context<Desktop>) {
        if !self.panel_shows(PanelTab::Device) {
            self.release_device_input();
            self.send_device_touch_end();
            return;
        }
        let Some(thread) = self.thread_id() else {
            return;
        };
        if self.panels.device.thread.as_deref() != Some(thread.as_str()) {
            self.release_device_input();
            self.send_device_touch_end();
            self.panels.device.thread = Some(thread.clone());
            self.panels.device.loaded = false;
            self.panels.device.subscribed = false;
            self.panels.device.detail_requests.clear();
            self.panels.device.accessibility_requests.clear();
            self.panels.device.event_log_requests.clear();
            self.panels.device.frames.clear();
            self.panels.device.frame_epochs.clear();
            self.panels.device.frame_bounds.clear();
            self.panels.device.active_touch = None;
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
        for session in self
            .snapshot
            .device()
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread)
        {
            let key = format!("{}:{}", session.host_id, session.device_id);
            if self.panels.device.detail_requests.insert(key) {
                self.perform(Intent::LoadDeviceDetail {
                    host_id: Some(session.host_id.clone()),
                    device_id: session.device_id.clone(),
                });
            }
            let accessibility_key = format!("{}:{}", session.host_id, session.device_id);
            if self
                .panels
                .device
                .accessibility_requests
                .insert(accessibility_key)
            {
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
        let active_decoder_sessions = view
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread)
            .map(|session| {
                (
                    session.thread_id.to_string(),
                    session.host_id.clone(),
                    session.device_id.clone(),
                    session.session_epoch.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        self.panels
            .device
            .decoder
            .retain_sessions(&active_decoder_sessions);
        let live_epochs = view
            .sessions
            .iter()
            .filter(|session| session.thread_id == thread)
            .map(|session| {
                (
                    (session.host_id.clone(), session.device_id.clone()),
                    session.session_epoch.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if self
            .panels
            .device
            .active_touch
            .as_ref()
            .is_some_and(|touch| {
                live_epochs.get(&(touch.host_id.clone(), touch.device_id.clone()))
                    != Some(&touch.session_epoch)
            })
        {
            self.send_device_touch_end();
        }
        if self
            .panels
            .device
            .keyboard_target
            .as_ref()
            .is_some_and(|(host_id, device_id, epoch)| {
                live_epochs.get(&(host_id.clone(), device_id.clone())) != Some(epoch)
            })
        {
            self.panels.device.keyboard_target = None;
        }
        self.panels
            .device
            .frame_epochs
            .retain(|(host_id, device_id, _), epoch| {
                live_epochs.get(&(host_id.clone(), device_id.clone())) == Some(epoch)
            });
        let live_frame_keys = self
            .panels
            .device
            .frame_epochs
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        self.panels
            .device
            .frames
            .retain(|key, _| live_frame_keys.contains(key));
        let mut newest_frames = BTreeMap::new();
        for frame in view.frames.iter().filter(|frame| frame.thread_id == thread) {
            let entry = newest_frames
                .entry((frame.host_id.clone(), frame.device_id.clone()))
                .or_insert(frame);
            if frame.sequence > entry.sequence {
                *entry = frame;
            }
        }
        for frame in newest_frames
            .into_values()
            .filter(|frame| !frame.png.is_empty())
        {
            let key = (frame.host_id.clone(), frame.device_id.clone(), 0);
            if self
                .panels
                .device
                .frames
                .get(&key)
                .is_none_or(|(sequence, _)| *sequence != frame.sequence)
            {
                self.panels.device.frames.insert(
                    key,
                    (
                        frame.sequence,
                        Arc::new(Image::from_bytes(ImageFormat::Png, frame.png.clone())),
                    ),
                );
                self.panels.device.frame_epochs.insert(
                    (frame.host_id.clone(), frame.device_id.clone(), 0),
                    frame.session_epoch.clone(),
                );
                cx.notify();
            }
        }
        let events = view
            .video_events
            .iter()
            .filter(|frame| frame.thread_id == thread)
            .cloned()
            .collect::<Vec<_>>();
        for image in self.panels.device.decoder.push(&events) {
            let format = match image.format {
                device_decoder::DeviceImageFormat::Jpeg => ImageFormat::Jpeg,
                device_decoder::DeviceImageFormat::Png => ImageFormat::Png,
            };
            self.panels.device.frames.insert(
                (
                    image.host_id.clone(),
                    image.device_id.clone(),
                    image.screen_id,
                ),
                (
                    image.sequence,
                    Arc::new(Image::from_bytes(format, image.bytes)),
                ),
            );
            self.panels.device.frame_epochs.insert(
                (
                    image.host_id.clone(),
                    image.device_id.clone(),
                    image.screen_id,
                ),
                image.session_epoch.clone(),
            );
            cx.notify();
        }
    }

    pub(super) fn render_device(&mut self, cx: &mut Context<Desktop>) -> AnyElement {
        let view = self.snapshot.device();
        let frame_sizes = view
            .video_frames
            .iter()
            .map(|frame| {
                (
                    (
                        frame.host_id.clone(),
                        frame.device_id.clone(),
                        frame.screen_id.unwrap_or(0),
                    ),
                    (frame.width, frame.height),
                )
            })
            .chain(view.frames.iter().map(|frame| {
                (
                    (frame.host_id.clone(), frame.device_id.clone(), 0),
                    (frame.width, frame.height),
                )
            }))
            .collect::<BTreeMap<_, _>>();
        let frame_images = self
            .panels
            .device
            .frames
            .values()
            .map(|((host_id, device_id, screen_id), (_, image))| {
                let (width, height) = frame_sizes
                    .get(&(host_id.clone(), device_id.clone(), *screen_id))
                    .copied()
                    .unwrap_or((1, 1));
                let session_epoch = self
                    .panels
                    .device
                    .frame_epochs
                    .get(&(host_id.clone(), device_id.clone(), *screen_id))
                    .cloned()
                    .unwrap_or_default();
                (
                    host_id.clone(),
                    device_id.clone(),
                    *screen_id,
                    session_epoch,
                    image.clone(),
                    width,
                    height,
                )
            })
            .collect::<Vec<_>>();
        let enabled = view.enabled;
        let owner = cx.entity().downgrade();
        let current_thread = self.thread_id();
        let action_targets = view
            .sessions
            .iter()
            .filter(|session| current_thread.as_deref() == Some(session.thread_id.as_str()))
            .map(|session| {
                (
                    session.host_id.clone(),
                    session.device_id.clone(),
                    session.platform.clone(),
                )
            })
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
                .id(SharedString::from(format!(
                    "device-{}-{}",
                    host_id, device_id
                )))
                .w_full()
                .h_8()
                .gap_2()
                .px_2()
                .rounded_md()
                .hover(|row| row.bg(tint("accentSurface", 0.6)))
                .child(
                    icon(if device.booted {
                        "circle-play"
                    } else {
                        "smartphone"
                    })
                    .size_4(),
                )
                .child(
                    v_flex().flex_1().min_w_0().child(
                        div()
                            .truncate()
                            .child(format!("{} · {}", device.name, device.platform)),
                    ),
                )
                .child(
                    Button::new(SharedString::from(format!("open-{host_id}-{device_id}")))
                        .label(if opened { "Close" } else { "Open" })
                        .small()
                        .on_click({
                            let owner = owner.clone();
                            move |_, _, cx| {
                                let _ = owner.update(cx, |view, _| {
                                    if opened {
                                        view.release_device_input_for(&host_id, &device_id);
                                        view.perform(Intent::CloseDevice {
                                            host_id: Some(host_id.clone()),
                                            device_id: Some(device_id.clone()),
                                            shutdown: false,
                                        });
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
                        }),
                )
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
                .child(
                    div()
                        .flex_1()
                        .text_2xs()
                        .text_color(color("textMuted"))
                        .child(format!("{}: {}", host.label, detail)),
                )
                .child(
                    Button::new(SharedString::from(format!("retry-device-host-{}", host.id)))
                        .label("Retry")
                        .xsmall()
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.perform(Intent::RetryDeviceHost {
                                host_id: retry_id.clone(),
                            })
                        })),
                )
                .when(host.target.is_some(), |row| {
                    let id = host.id.clone();
                    row.child(
                        Button::new(SharedString::from(format!("remove-device-host-{id}")))
                            .label("Remove")
                            .xsmall()
                            .on_click(
                                cx.listener(move |view, _, _, _| view.remove_device_ssh_host(&id)),
                            ),
                    )
                })
        });
        let device_controls =
            action_targets
                .into_iter()
                .map(|(host_id, device_id, platform)| {
                    let target = format!("{host_id}:{device_id}");
                    let is_ios = platform == "ios";
                    let is_android = platform == "android";
                    let duo_status =
                        view.duo_controls
                            .iter()
                            .find(|control| {
                                current_thread.as_deref() == Some(control.thread_id.as_str())
                                    && control.host_id == host_id
                                    && control.device_id == device_id
                            })
                            .and_then(|control| {
                                control.error.clone().or_else(|| {
                                    control.pending.then_some("Duo control pending".into())
                                })
                            });
                    h_flex()
                        .id(SharedString::from(format!("device-controls-{target}")))
                        .gap_1()
                        .child(
                            div()
                                .text_2xs()
                                .text_color(color("textMuted"))
                                .child(target),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-dark-{host_id}-{device_id}"
                            )))
                            .label("Dark appearance")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::SetAppearance { dark: true },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-text-large-{host_id}-{device_id}"
                            )))
                            .label("Larger text")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::SetTextSize {
                                            size: "large".into(),
                                        },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-motion-{host_id}-{device_id}"
                            )))
                            .label("Reduce motion")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::SetToggle {
                                            setting: "reduceMotion".into(),
                                            value: true,
                                        },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-light-{host_id}-{device_id}"
                            )))
                            .label("Light appearance")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::SetAppearance { dark: false },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-home-{host_id}-{device_id}"
                            )))
                            .label("Home")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::HardwareButton {
                                            button: "home".into(),
                                        },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-back-{host_id}-{device_id}"
                            )))
                            .label("Back")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::HardwareButton {
                                            button: "back".into(),
                                        },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-recents-{host_id}-{device_id}"
                            )))
                            .label("Recents")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::HardwareButton {
                                            button: "recents".into(),
                                        },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-power-{host_id}-{device_id}"
                            )))
                            .label("Power")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::HardwareButton {
                                            button: "power".into(),
                                        },
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-enter-{host_id}-{device_id}"
                            )))
                            .label("Enter")
                            .xsmall()
                            .on_click(cx.listener({
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
                                            shift: false,
                                            alt: false,
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
                                            shift: false,
                                            alt: false,
                                        },
                                    });
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-touch-center-{host_id}-{device_id}"
                            )))
                            .label("Touch center")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::Touch {
                                            phase: "begin".into(),
                                            x: 0.5,
                                            y: 0.5,
                                        },
                                    });
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::Touch {
                                            phase: "end".into(),
                                            x: 0.5,
                                            y: 0.5,
                                        },
                                    });
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-rotate-{host_id}-{device_id}"
                            )))
                            .label("Rotate")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::DeviceAction {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        action: DeviceActionIntent::Rotate,
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-screenshot-{host_id}-{device_id}"
                            )))
                            .label("Screenshot")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::CaptureDeviceScreenshot {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                    })
                                }
                            })),
                        )
                        .when(is_ios, |row| {
                            let commands = [
                                (
                                    "Book",
                                    DeviceDuoCommandIntent::Pose {
                                        value: DeviceDuoPoseIntent::Book,
                                    },
                                ),
                                (
                                    "Closed",
                                    DeviceDuoCommandIntent::Pose {
                                        value: DeviceDuoPoseIntent::Closed,
                                    },
                                ),
                                (
                                    "Open",
                                    DeviceDuoCommandIntent::Pose {
                                        value: DeviceDuoPoseIntent::Open,
                                    },
                                ),
                                (
                                    "Laptop",
                                    DeviceDuoCommandIntent::Pose {
                                        value: DeviceDuoPoseIntent::Laptop,
                                    },
                                ),
                                (
                                    "Tent",
                                    DeviceDuoCommandIntent::Pose {
                                        value: DeviceDuoPoseIntent::Tent,
                                    },
                                ),
                                ("Table on", DeviceDuoCommandIntent::Table { value: true }),
                                ("Table off", DeviceDuoCommandIntent::Table { value: false }),
                                (
                                    "Face up",
                                    DeviceDuoCommandIntent::Physical {
                                        value: DeviceDuoPhysicalIntent::Faceup,
                                    },
                                ),
                                (
                                    "Face down",
                                    DeviceDuoCommandIntent::Physical {
                                        value: DeviceDuoPhysicalIntent::Facedown,
                                    },
                                ),
                                ("0°", DeviceDuoCommandIntent::Angle { value: 0.0 }),
                                ("45°", DeviceDuoCommandIntent::Angle { value: 45.0 }),
                                ("90°", DeviceDuoCommandIntent::Angle { value: 90.0 }),
                                ("135°", DeviceDuoCommandIntent::Angle { value: 135.0 }),
                                ("180°", DeviceDuoCommandIntent::Angle { value: 180.0 }),
                                (
                                    "Portrait",
                                    DeviceDuoCommandIntent::Orientation {
                                        value: DeviceDuoOrientationIntent::Portrait,
                                    },
                                ),
                                (
                                    "Landscape left",
                                    DeviceDuoCommandIntent::Orientation {
                                        value: DeviceDuoOrientationIntent::LandscapeLeft,
                                    },
                                ),
                                (
                                    "Upside down",
                                    DeviceDuoCommandIntent::Orientation {
                                        value: DeviceDuoOrientationIntent::PortraitUpsideDown,
                                    },
                                ),
                                (
                                    "Landscape right",
                                    DeviceDuoCommandIntent::Orientation {
                                        value: DeviceDuoOrientationIntent::LandscapeRight,
                                    },
                                ),
                            ];
                            row.children(commands.into_iter().enumerate().map(
                                |(index, (label, command))| {
                                    let host_id = host_id.clone();
                                    let device_id = device_id.clone();
                                    Button::new(SharedString::from(format!(
                                        "device-duo-{index}-{host_id}-{device_id}"
                                    )))
                                    .label(label)
                                    .xsmall()
                                    .on_click(cx.listener(move |view, _, _, _| {
                                        view.perform(Intent::DeviceAction {
                                            host_id: Some(host_id.clone()),
                                            device_id: device_id.clone(),
                                            action: DeviceActionIntent::Duo {
                                                command: command.clone(),
                                            },
                                        })
                                    }))
                                },
                            ))
                        })
                        .when(is_android, |row| {
                            let commands = [
                                ("Fold closed", DeviceFoldPostureIntent::Closed),
                                ("Fold open", DeviceFoldPostureIntent::Opened),
                            ];
                            row.children(commands.into_iter().enumerate().map(
                                |(index, (label, command))| {
                                    let host_id = host_id.clone();
                                    let device_id = device_id.clone();
                                    Button::new(SharedString::from(format!(
                                        "device-fold-{index}-{host_id}-{device_id}"
                                    )))
                                    .label(label)
                                    .xsmall()
                                    .on_click(cx.listener(move |view, _, _, _| {
                                        view.perform(Intent::DeviceAction {
                                            host_id: Some(host_id.clone()),
                                            device_id: device_id.clone(),
                                            action: DeviceActionIntent::Fold { command },
                                        })
                                    }))
                                },
                            ))
                        })
                        .when_some(duo_status, |row, status| {
                            row.child(
                                div()
                                    .text_2xs()
                                    .text_color(color("textMuted"))
                                    .child(status),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-record-{host_id}-{device_id}"
                            )))
                            .label("Record")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::StartDeviceRecording {
                                        host_id: Some(host_id.clone()),
                                        device_id: device_id.clone(),
                                        format: "mp4".into(),
                                    })
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-stop-record-{host_id}-{device_id}"
                            )))
                            .label("Stop record")
                            .xsmall()
                            .on_click(cx.listener({
                                let host_id = host_id.clone();
                                let device_id = device_id.clone();
                                move |view, _, _, _| {
                                    let recording =
                                        view.snapshot.device().recordings.into_iter().find(
                                            |recording| {
                                                recording.host_id == host_id
                                                    && recording.device_id == device_id
                                                    && recording.thread_id
                                                        == view.thread_id().unwrap_or_default()
                                            },
                                        );
                                    if let Some(recording) = recording {
                                        view.perform(Intent::StopDeviceRecording {
                                            host_id: Some(host_id.clone()),
                                            device_id: device_id.clone(),
                                            recording_id: recording.recording_id,
                                            session_epoch: recording.session_epoch,
                                        });
                                    }
                                }
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "device-power-off-{host_id}-{device_id}"
                            )))
                            .label("Power off")
                            .xsmall()
                            .on_click(cx.listener({
                                move |view, _, _, _| {
                                    view.release_device_input_for(&host_id, &device_id);
                                    view.perform(Intent::CloseDevice {
                                        host_id: Some(host_id.clone()),
                                        device_id: Some(device_id.clone()),
                                        shutdown: true,
                                    })
                                }
                            })),
                        )
                });
        v_flex()
            .id("device-panel")
            .flex_1()
            .min_h_0()
            .gap_2()
            .p_3()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Device"),
                    )
                    .child(
                        Button::new("device-refresh")
                            .label("Refresh")
                            .small()
                            .on_click(
                                cx.listener(|view, _, _, _| view.perform(Intent::LoadDevices)),
                            ),
                    ),
            )
            .when(!enabled, |panel| {
                panel.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child("Device support is off."))
                        .child(
                            Button::new("device-enable")
                                .label("Enable device support")
                                .small()
                                .on_click(cx.listener(|view, _, _, _| {
                                    view.perform(Intent::ConfigureDevices {
                                        enabled: Some(true),
                                        agent_access_enabled: None,
                                        onboarding_completed: Some(false),
                                    });
                                })),
                        ),
                )
            })
            .when(enabled, |panel| {
                panel
                    .child(
                        div()
                            .text_2xs()
                            .text_color(color("textMuted"))
                            .child(format!("Host status: {}", view.status)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("device-inspect-tools")
                                    .label("Inspect tools")
                                    .xsmall()
                                    .on_click(cx.listener(|view, _, _, _| {
                                        view.perform(Intent::InspectDevices { host_id: None })
                                    })),
                            )
                            .child(
                                Button::new("device-update-hub")
                                    .label("Update hub")
                                    .xsmall()
                                    .on_click(cx.listener(|view, _, _, _| {
                                        view.perform(Intent::UpdateDeviceTool {
                                            host_id: None,
                                            tool: "hub".into(),
                                        })
                                    })),
                            )
                            .child(
                                Button::new("device-update-agent")
                                    .label("Update agent")
                                    .xsmall()
                                    .on_click(cx.listener(|view, _, _, _| {
                                        view.perform(Intent::UpdateDeviceTool {
                                            host_id: None,
                                            tool: "agent".into(),
                                        })
                                    })),
                            ),
                    )
                    .when_some(view.status_detail.clone(), |panel, detail| {
                        panel.child(
                            div()
                                .text_2xs()
                                .text_color(color("textMuted"))
                                .child(detail),
                        )
                    })
                    .child(v_flex().gap_0p5().children(hosts))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("SSH device host"),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Input::new(&self.panels.device.ssh_label)
                                            .small()
                                            .flex_1()
                                            .aria_label("SSH host label"),
                                    )
                                    .child(
                                        Input::new(&self.panels.device.ssh_target)
                                            .small()
                                            .flex_1()
                                            .aria_label("SSH host target"),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Input::new(&self.panels.device.ssh_identity_file)
                                            .small()
                                            .flex_1()
                                            .aria_label("SSH identity file"),
                                    )
                                    .child(
                                        Input::new(&self.panels.device.ssh_port)
                                            .small()
                                            .w(px(64.))
                                            .aria_label("SSH port"),
                                    )
                                    .child(
                                        Button::new("device-add-ssh")
                                            .label("Save host")
                                            .small()
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.configure_device_ssh_host(cx)
                                            })),
                                    ),
                            ),
                    )
                    .when(!view.agent_access_enabled, |panel| {
                        panel.child(
                            Button::new("device-agent-enable")
                                .label("Enable agent device access")
                                .small()
                                .on_click(cx.listener(|view, _, _, _| {
                                    view.perform(Intent::ConfigureDevices {
                                        enabled: None,
                                        agent_access_enabled: Some(true),
                                        onboarding_completed: Some(true),
                                    });
                                })),
                        )
                    })
                    .child(v_flex().gap_0p5().children(entries))
                    .when(current_thread_has_session, |panel| {
                        panel.child(v_flex().gap_0p5().children(device_controls))
                    })
                    .child(
                        v_flex().gap_0p5().children(
                            view.foreground
                                .iter()
                                .filter(|foreground| {
                                    current_thread.as_deref().is_some_and(|thread| {
                                        view.sessions.iter().any(|session| {
                                            session.thread_id == thread
                                                && session.host_id == foreground.host_id
                                                && session.device_id == foreground.device_id
                                        })
                                    })
                                })
                                .map(|foreground| {
                                    div()
                                        .text_2xs()
                                        .text_color(color("textMuted"))
                                        .child(format!(
                                            "Foreground {}:{} · {}",
                                            foreground.host_id,
                                            foreground.device_id,
                                            foreground.app_id.as_deref().unwrap_or("unknown")
                                        ))
                                }),
                        ),
                    )
                    .child(
                        v_flex().gap_0p5().children(
                            view.screens
                                .iter()
                                .filter(|screen| {
                                    current_thread.as_deref().is_some_and(|thread| {
                                        screen.thread_id.as_deref() == Some(thread)
                                    })
                                })
                                .map(|screen| {
                                    div()
                                        .text_2xs()
                                        .text_color(color("textMuted"))
                                        .child(format!(
                                            "Screen {} · {}×{} · {}{}",
                                            screen
                                                .screen_id
                                                .map_or_else(|| "main".into(), |id| id.to_string()),
                                            screen.width,
                                            screen.height,
                                            screen.orientation,
                                            screen
                                                .hinge_pose
                                                .as_deref()
                                                .map_or(String::new(), |pose| format!(" · {pose}"))
                                        ))
                                }),
                        ),
                    )
                    .when(!view.accessibility.is_empty(), |panel| {
                        panel.child(
                            v_flex()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Accessibility overlay"),
                                )
                                .children(view.accessibility.iter().flat_map(|tree| {
                                    tree.elements
                                        .iter()
                                        .filter(|element| !element.label.is_empty())
                                        .take(20)
                                        .map(|element| {
                                            div().text_2xs().text_color(color("textMuted")).child(
                                                format!("{} · {}", element.role, element.label),
                                            )
                                        })
                                })),
                        )
                    })
                    .when(!view.event_log.is_empty(), |panel| {
                        panel.child(
                            v_flex()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Device event log"),
                                )
                                .children(view.event_log.iter().rev().take(20).map(|entry| {
                                    div()
                                        .text_2xs()
                                        .text_color(color("textMuted"))
                                        .child(format!("{} · {}", entry.kind, entry.summary))
                                })),
                        )
                    })
                    .when_some(
                        view.last_recording.clone().filter(|recording| {
                            current_thread.as_deref() == Some(recording.thread_id.as_str())
                        }),
                        |panel, recording| {
                            let recording_for_attach = recording.clone();
                            let draft_key = self.snapshot.draft_key();
                            panel.child(
                                h_flex()
                                    .gap_1()
                                    .child(div().text_2xs().text_color(color("textMuted")).child(
                                        format!(
                                            "Recording ready · {} frames · {} bytes",
                                            recording.frame_count, recording.byte_count
                                        ),
                                    ))
                                    .child(
                                        Button::new("device-attach-recording")
                                            .label("Attach recording")
                                            .xsmall()
                                            .on_click(cx.listener(move |view, _, _, _| {
                                                view.attach_device_recording(
                                                    draft_key.clone(),
                                                    recording_for_attach.clone(),
                                                )
                                            })),
                                    ),
                            )
                        },
                    )
                    .when_some(
                        self.panels.device.decoder.error().map(str::to_owned),
                        |panel, error| {
                            panel
                                .child(div().text_2xs().text_color(color("textMuted")).child(error))
                        },
                    )
                    .child(if frame_images.is_empty() {
                        div()
                            .flex_1()
                            .min_h_0()
                            .items_center()
                            .justify_center()
                            .text_color(color("textMuted"))
                            .child("Open a device to see its live frame")
                            .into_any_element()
                    } else {
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .children(frame_images.into_iter().map(
                                |(
                                    host_id,
                                    device_id,
                                    screen_id,
                                    session_epoch,
                                    image,
                                    frame_width,
                                    frame_height,
                                )| {
                                    let frame_key = (host_id.clone(), device_id.clone(), screen_id);
                                    let bounds_owner = owner.clone();
                                    let down_host = host_id.clone();
                                    let down_device = device_id.clone();
                                    let down_epoch = session_epoch.clone();
                                    let move_host = host_id.clone();
                                    let move_device = device_id.clone();
                                    let move_epoch = session_epoch.clone();
                                    let up_host = host_id.clone();
                                    let up_device = device_id.clone();
                                    let up_epoch = session_epoch.clone();
                                    let out_host = host_id.clone();
                                    let out_device = device_id.clone();
                                    let exit_host = host_id.clone();
                                    let exit_device = device_id.clone();
                                    let key_down_epoch = session_epoch.clone();
                                    let key_up_epoch = session_epoch.clone();
                                    let key_down_host = host_id.clone();
                                    let key_down_device = device_id.clone();
                                    let key_up_host = host_id.clone();
                                    let key_up_device = device_id.clone();
                                    let prepaint_key = frame_key.clone();
                                    let accessibility = view
                                        .accessibility
                                        .iter()
                                        .filter(|tree| {
                                            tree.host_id == host_id && tree.device_id == device_id
                                        })
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
                                    div()
                                        .id(SharedString::from(format!(
                                            "device-frame-{host_id}-{device_id}-{screen_id}"
                                        )))
                                        .relative()
                                        .flex_1()
                                        .min_h_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .on_prepaint(move |bounds, _, cx| {
                                            let _ = bounds_owner.update(cx, |view, cx| {
                                                if view
                                                    .panels
                                                    .device
                                                    .frame_bounds
                                                    .get(&prepaint_key)
                                                    != Some(&bounds)
                                                {
                                                    view.panels
                                                        .device
                                                        .frame_bounds
                                                        .insert(prepaint_key.clone(), bounds);
                                                    cx.notify();
                                                }
                                            });
                                        })
                                        .track_focus(&self.panels.device.focus)
                                        .on_key_down(cx.listener(
                                            move |view, event: &KeyDownEvent, _, cx| {
                                                view.device_key_down(
                                                    key_down_host.clone(),
                                                    key_down_device.clone(),
                                                    key_down_epoch.clone(),
                                                    event,
                                                    cx,
                                                );
                                            },
                                        ))
                                        .on_key_up(cx.listener(
                                            move |view, event: &KeyUpEvent, _, cx| {
                                                view.device_key_up(
                                                    key_up_host.clone(),
                                                    key_up_device.clone(),
                                                    key_up_epoch.clone(),
                                                    event,
                                                    cx,
                                                );
                                            },
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |view, event: &MouseDownEvent, _, cx| {
                                                    view.device_touch(
                                                        down_host.clone(),
                                                        down_device.clone(),
                                                        screen_id,
                                                        down_epoch.clone(),
                                                        "begin",
                                                        event.position,
                                                        frame_width,
                                                        frame_height,
                                                        cx,
                                                    );
                                                },
                                            ),
                                        )
                                        .on_mouse_move(cx.listener(
                                            move |view, event: &MouseMoveEvent, _, cx| {
                                                if view
                                                    .panels
                                                    .device
                                                    .active_touch
                                                    .as_ref()
                                                    .is_some_and(|touch| {
                                                        touch.host_id == move_host
                                                            && touch.device_id == move_device
                                                            && touch.screen_id == screen_id
                                                    })
                                                {
                                                    view.device_touch(
                                                        move_host.clone(),
                                                        move_device.clone(),
                                                        screen_id,
                                                        move_epoch.clone(),
                                                        "move",
                                                        event.position,
                                                        frame_width,
                                                        frame_height,
                                                        cx,
                                                    );
                                                }
                                            },
                                        ))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |view, event: &MouseUpEvent, _, cx| {
                                                    if view
                                                        .panels
                                                        .device
                                                        .active_touch
                                                        .as_ref()
                                                        .is_some_and(|touch| {
                                                            touch.host_id == up_host
                                                                && touch.device_id == up_device
                                                                && touch.screen_id == screen_id
                                                        })
                                                    {
                                                        view.device_touch(
                                                            up_host.clone(),
                                                            up_device.clone(),
                                                            screen_id,
                                                            up_epoch.clone(),
                                                            "end",
                                                            event.position,
                                                            frame_width,
                                                            frame_height,
                                                            cx,
                                                        );
                                                    }
                                                },
                                            ),
                                        )
                                        .on_mouse_up_out(
                                            MouseButton::Left,
                                            cx.listener(move |view, _, _, cx| {
                                                if view
                                                    .panels
                                                    .device
                                                    .active_touch
                                                    .as_ref()
                                                    .is_some_and(|touch| {
                                                        touch.host_id == out_host
                                                            && touch.device_id == out_device
                                                            && touch.screen_id == screen_id
                                                    })
                                                {
                                                    view.cancel_device_touch(cx);
                                                }
                                            }),
                                        )
                                        .on_mouse_exit(cx.listener(move |view, _, _, cx| {
                                            if view.panels.device.active_touch.as_ref().is_some_and(
                                                |touch| {
                                                    touch.host_id == exit_host
                                                        && touch.device_id == exit_device
                                                        && touch.screen_id == screen_id
                                                },
                                            ) {
                                                view.cancel_device_touch(cx);
                                            }
                                        }))
                                        .child(
                                            img(image)
                                                .max_w_full()
                                                .max_h_full()
                                                .object_fit(ObjectFit::Contain),
                                        )
                                        .children(accessibility)
                                },
                            ))
                            .into_any_element()
                    })
            })
            .into_any_element()
    }
}

fn device_frame_point(
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    frame_width: u32,
    frame_height: u32,
) -> Option<(f32, f32)> {
    let view_width = bounds.size.width.as_f32().max(1.0);
    let view_height = bounds.size.height.as_f32().max(1.0);
    let point = agent_core::view::device::project_touch_point(
        position.x.as_f32() - bounds.left().as_f32(),
        position.y.as_f32() - bounds.top().as_f32(),
        view_width,
        view_height,
        frame_width as f32,
        frame_height as f32,
    )?;
    Some((point.x, point.y))
}

fn recording_file_type(
    recording: &agent_core::view::device::DeviceRecordingView,
) -> Option<(&'static str, String)> {
    let extension = std::path::Path::new(&recording.file_name)
        .extension()
        .and_then(|extension| extension.to_str())?;
    if !extension.eq_ignore_ascii_case("mp4")
        || !recording.mime_type.eq_ignore_ascii_case("video/mp4")
        || recording.bytes.len() < 12
        || &recording.bytes[4..8] != b"ftyp"
    {
        return None;
    }
    Some(("mp4", recording.mime_type.clone()))
}
