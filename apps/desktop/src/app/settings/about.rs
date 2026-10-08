//! About and update actions. The page renders core's pure update state; the
//! Host owns download and install effects and reports when a handoff is needed.
use super::{Choice, Row, page_container, section, select};
use crate::app::{Desktop, ui::color};
use agent_core::state::Intent;
use agent_protocol::models::{
    UpdateChannel, UpdateCheckRequest, UpdateState, UpdateStatus, UpdateTarget,
};
use gpui_kit::{
    component::{Disableable, Sizable, button::Button, h_flex},
    prelude::FluentBuilder,
    *,
};

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn channel_label(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Nightly => "Nightly",
        UpdateChannel::Preview => "Preview",
        UpdateChannel::Stable => "Stable",
    }
}

pub(super) fn default_channel() -> UpdateChannel {
    match option_env!("APP_RELEASE_CHANNEL") {
        Some("nightly") => UpdateChannel::Nightly,
        Some("preview") => UpdateChannel::Preview,
        _ => UpdateChannel::Stable,
    }
}

fn current_version() -> String {
    std::env::var("APP_UPDATE_VERSION")
        .ok()
        .or_else(|| option_env!("APP_UPDATE_VERSION").map(str::to_owned))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

fn status_label(state: Option<&UpdateState>) -> String {
    let Some(state) = state else {
        return "Not checked".into();
    };
    if state.restart_required {
        return "Restart the app to finish installing".into();
    }
    match state.status {
        UpdateStatus::Disabled => state
            .message
            .clone()
            .unwrap_or_else(|| "Updates are unavailable".into()),
        UpdateStatus::Idle => "Ready to check".into(),
        UpdateStatus::Checking => "Checking for updates…".into(),
        UpdateStatus::Available => state.available_version.as_ref().map_or_else(
            || "Update available".into(),
            |version| format!("{version} is available"),
        ),
        UpdateStatus::Downloading => state.download_percent.map_or_else(
            || "Downloading…".into(),
            |percent| format!("Downloading ({percent}%)"),
        ),
        UpdateStatus::Downloaded => state.downloaded_version.as_ref().map_or_else(
            || "Ready to install".into(),
            |version| format!("{version} is ready to install"),
        ),
        UpdateStatus::Installing => "Installing…".into(),
        UpdateStatus::UpToDate => "Up to date".into(),
        UpdateStatus::Error => state
            .message
            .clone()
            .unwrap_or_else(|| "Update check failed".into()),
    }
}

pub(super) fn check_request(target: UpdateTarget, channel: UpdateChannel) -> UpdateCheckRequest {
    UpdateCheckRequest {
        target,
        current_version: current_version(),
        channel,
        platform: platform().into(),
        architecture: std::env::consts::ARCH.into(),
    }
}

impl Desktop {
    pub(super) fn render_about(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let host = self.snapshot.updates.get(&UpdateTarget::Host);
        let desktop = self.snapshot.updates.get(&UpdateTarget::Desktop);
        let channel = host
            .or(desktop)
            .map_or_else(default_channel, |state| state.channel);
        let channel_choices = [
            UpdateChannel::Stable,
            UpdateChannel::Nightly,
            UpdateChannel::Preview,
        ]
        .into_iter()
        .map(|value| Choice {
            id: format!("{value:?}").to_lowercase(),
            label: channel_label(value).into(),
            description: None,
            icon: None,
            selected: value == channel,
        })
        .collect();
        let channel_control = select(
            "update-channel",
            channel_label(channel),
            channel_choices,
            move |view, value, _, _| {
                let selected = match value.as_str() {
                    "nightly" => UpdateChannel::Nightly,
                    "preview" => UpdateChannel::Preview,
                    _ => UpdateChannel::Stable,
                };
                view.perform(Intent::SetUpdateChannel {
                    target: UpdateTarget::Host,
                    channel: selected,
                });
                view.perform(Intent::SetUpdateChannel {
                    target: UpdateTarget::Desktop,
                    channel: selected,
                });
            },
            cx,
        );
        let version = current_version();
        page_container(
            896.,
            vec![
                section(
                    Some("About".into()),
                    None,
                    None,
                    vec![
                        Row::new("Version")
                            .description("The desktop and Host protocol versions are built from the same revision.")
                            .control(div().text_sm().text_color(color("textMuted")).child(version))
                            .render(),
                        Row::new("Release channel")
                            .description("Checks use the selected channel's release manifest.")
                            .control(channel_control)
                            .render(),
                    ],
                )
                .into_any_element(),
                section(
                    Some("Updates".into()),
                    None,
                    None,
                    vec![self.render_update_row(UpdateTarget::Host, "Host", host, cx), self.render_update_row(UpdateTarget::Desktop, "Desktop", desktop, cx)],
                )
                .into_any_element(),
            ],
        )
    }

    fn render_update_row(
        &self,
        target: UpdateTarget,
        label: &'static str,
        state: Option<&UpdateState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = status_label(state);
        let channel = state.map_or(UpdateChannel::Stable, |state| state.channel);
        let request = check_request(target, channel);
        let action = match state.map(|state| state.status) {
            Some(_) if state.is_some_and(|state| state.restart_required) => None,
            Some(UpdateStatus::Available) => Some(Intent::DownloadUpdate { target }),
            Some(UpdateStatus::Downloaded) => Some(Intent::InstallUpdate { target }),
            Some(UpdateStatus::Checking | UpdateStatus::Downloading | UpdateStatus::Installing) => {
                None
            }
            _ => Some(Intent::CheckUpdate { request }),
        };
        let button_label = match state.map(|state| state.status) {
            Some(_) if state.is_some_and(|state| state.restart_required) => "Restart required",
            Some(UpdateStatus::Available) => "Download",
            Some(UpdateStatus::Downloaded) => "Install",
            Some(UpdateStatus::Checking | UpdateStatus::Downloading | UpdateStatus::Installing) => {
                "Working…"
            }
            _ => "Check",
        };
        let button = Button::new(SharedString::from(format!("update-{target:?}")))
            .small()
            .label(button_label)
            .disabled(action.is_none())
            .on_click(cx.listener(move |view, _, _, _| {
                if let Some(intent) = action.clone() {
                    view.perform(intent);
                }
            }));
        Row::new(label)
            .description(status)
            .control(h_flex().gap(px(8.)).child(button))
            .render()
    }
}
