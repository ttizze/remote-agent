//! Pure Preview presentation and viewport decisions.
use crate::state::{PreviewState, Snapshot};
use agent_protocol::preview::{
    DiscoveredLocalServer, PREVIEW_VIEWPORT_PRESETS, PreviewSessionSnapshot, PreviewViewportPreset,
    PreviewViewportSetting, PreviewZoom, validate_viewport_dimensions,
};

/// Resolves the device-local browser defaults for a Preview opener. The Host
/// receives the resulting explicit viewport/appearance/zoom/profile values;
/// it does not read project overrides for these client-owned settings.
pub fn browser_defaults(snapshot: &Snapshot) -> crate::view::browser::BrowserDefaults {
    snapshot.preferences.browser.resolved()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewView {
    pub sessions: Vec<PreviewSessionSnapshot>,
    pub local_servers: Vec<DiscoveredLocalServer>,
    pub recent_urls: Vec<String>,
    pub active_tab: Option<String>,
    pub server_epoch: Option<String>,
    pub revision: u64,
    pub scanner_epoch: Option<String>,
    pub scanner_revision: u64,
    pub empty_state: PreviewEmptyState,
    pub viewport_presets: Vec<PreviewViewportPresetView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewEmptyState {
    NoTab,
    NoLocalServers,
    LocalServers,
    Tabs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewViewportPresetView {
    pub id: PreviewViewportPreset,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
}

pub fn preview(snapshot: &Snapshot) -> PreviewView {
    preview_state(&snapshot.preview)
}

pub fn preview_state(state: &PreviewState) -> PreviewView {
    let sessions: Vec<_> = state.sessions.values().cloned().collect();
    PreviewView {
        sessions,
        local_servers: state.local_servers.clone(),
        recent_urls: state.recent_urls.clone(),
        active_tab: state.active_tab.clone(),
        server_epoch: state.server_epoch.clone(),
        revision: state.revision,
        scanner_epoch: state.scanner_epoch.clone(),
        scanner_revision: state.scanner_revision,
        empty_state: if state.sessions.is_empty() {
            if state.local_servers.is_empty() {
                PreviewEmptyState::NoTab
            } else {
                PreviewEmptyState::LocalServers
            }
        } else if state.local_servers.is_empty() {
            PreviewEmptyState::NoLocalServers
        } else {
            PreviewEmptyState::Tabs
        },
        viewport_presets: PREVIEW_VIEWPORT_PRESETS
            .iter()
            .map(|preset| PreviewViewportPresetView {
                id: preset.id,
                label: preset.label,
                width: preset.width,
                height: preset.height,
            })
            .collect(),
    }
}

/// Resolves fill against the actual panel resource dimensions.  Device
/// presets and freeform values are validated by the shared contract first.
pub fn resolve_viewport(
    setting: PreviewViewportSetting,
    resource_width: u32,
    resource_height: u32,
) -> Result<(u32, u32), String> {
    match setting {
        PreviewViewportSetting::Fill => {
            if resource_width == 0 || resource_height == 0 {
                return Err("preview resource dimensions are empty".into());
            }
            Ok((resource_width, resource_height))
        }
        PreviewViewportSetting::Freeform { width, height }
        | PreviewViewportSetting::Preset { width, height, .. } => {
            validate_viewport_dimensions(width, height)?;
            Ok((width, height))
        }
    }
}

pub fn zoom_step(zoom: PreviewZoom, direction: i8) -> PreviewZoom {
    zoom.stepped(direction)
}

pub fn sort_local_servers(servers: &mut [DiscoveredLocalServer]) {
    servers.sort_by(|left, right| {
        left.port
            .cmp(&right.port)
            .then_with(|| left.url.cmp(&right.url))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_uses_resource_dimensions_and_freeform_uses_its_own_dimensions() {
        assert_eq!(
            resolve_viewport(PreviewViewportSetting::Fill, 732, 611).unwrap(),
            (732, 611)
        );
        assert_eq!(
            resolve_viewport(
                PreviewViewportSetting::Freeform {
                    width: 390,
                    height: 844
                },
                1,
                1
            )
            .unwrap(),
            (390, 844)
        );
    }

    #[test]
    fn preview_empty_state_distinguishes_server_cards_from_no_results() {
        assert_eq!(
            preview_state(&PreviewState::default()).empty_state,
            PreviewEmptyState::NoTab
        );
    }
}
