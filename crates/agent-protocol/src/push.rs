//! Native push registration and the small activity record sent to a device.
//!
//! The Host owns provider credentials and delivery.  Clients only keep their
//! platform token, the presentation preferences, and enough routing metadata
//! to open the correct thread when an operating-system notification is tapped.

use agent_domain::{ActivityContentState, ActivityRecord};
use serde::{Deserialize, Serialize};

const MAX_DEVICE_ID_BYTES: usize = 128;
const MAX_TOKEN_BYTES: usize = 4096;
const MAX_BUNDLE_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushPlatform {
    Ios,
    Android,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApnsEnvironment {
    Sandbox,
    Production,
}

/// Presentation choices stored with a Host-side device registration. Native
/// clients pass provider capability and OS authorization facts; core combines
/// those facts with the persisted Live Activities preference and the source's
/// fixed per-event notification policy before constructing this contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PushPreferences {
    pub notifications_enabled: bool,
    pub notify_on_approval: bool,
    pub notify_on_input: bool,
    pub notify_on_completion: bool,
    pub notify_on_failure: bool,
    pub live_activities_enabled: bool,
}

impl Default for PushPreferences {
    fn default() -> Self {
        Self {
            notifications_enabled: true,
            notify_on_approval: true,
            notify_on_input: true,
            notify_on_completion: true,
            notify_on_failure: true,
            live_activities_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterPushDevice {
    pub device_id: String,
    pub platform: PushPlatform,
    pub token: String,
    pub live_activity_token: Option<String>,
    pub push_to_start_token: Option<String>,
    pub bundle_id: Option<String>,
    pub apns_environment: Option<ApnsEnvironment>,
    pub preferences: PushPreferences,
}

impl RegisterPushDevice {
    pub fn validate(&self) -> Result<(), String> {
        validate_text(&self.device_id, "device id", MAX_DEVICE_ID_BYTES)?;
        validate_text(&self.token, "push token", MAX_TOKEN_BYTES)?;
        if let Some(token) = &self.live_activity_token {
            validate_text(token, "live activity token", MAX_TOKEN_BYTES)?;
        }
        if let Some(token) = &self.push_to_start_token {
            validate_text(token, "push to start token", MAX_TOKEN_BYTES)?;
        }
        if let Some(bundle_id) = &self.bundle_id {
            validate_text(bundle_id, "bundle id", MAX_BUNDLE_ID_BYTES)?;
        }
        match self.platform {
            PushPlatform::Ios => {
                if self.apns_environment.is_none() {
                    return Err("iOS push registration needs an APNs environment".into());
                }
                let Some(bundle_id) = self.bundle_id.as_deref() else {
                    return Err("iOS push registration needs a bundle id".into());
                };
                if !bundle_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
                {
                    return Err("iOS bundle id contains an invalid character".into());
                }
            }
            PushPlatform::Android => {
                if self.live_activity_token.is_some()
                    || self.push_to_start_token.is_some()
                    || self.apns_environment.is_some()
                    || self.bundle_id.is_some()
                {
                    return Err("Android push registration cannot contain APNs fields".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushDeviceId {
    pub device_id: String,
}

impl PushDeviceId {
    pub fn validate(&self) -> Result<(), String> {
        validate_text(&self.device_id, "device id", MAX_DEVICE_ID_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPushDeviceActive {
    pub device_id: String,
    pub active: bool,
}

impl SetPushDeviceActive {
    pub fn validate(&self) -> Result<(), String> {
        validate_text(&self.device_id, "device id", MAX_DEVICE_ID_BYTES)
    }
}

/// A Host-side activity transition.  It is intentionally provider-neutral so
/// APNs and FCM stay thin translators over one routing decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushActivityPhase {
    Starting,
    Running,
    WaitingForApproval,
    WaitingForInput,
    Completed,
    Failed,
    Stale,
}

impl PushActivityPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stale)
    }

    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingForApproval => "waiting_for_approval",
            Self::WaitingForInput => "waiting_for_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }

    pub fn from_wire(value: &str) -> Self {
        match value {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "waiting_for_approval" => Self::WaitingForApproval,
            "waiting_for_input" => Self::WaitingForInput,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Stale,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushActivityEvent {
    pub host_id: String,
    pub thread_id: String,
    pub project_id: String,
    pub project_title: String,
    pub thread_title: String,
    pub model_title: String,
    pub phase: PushActivityPhase,
    pub headline: String,
    pub detail: Option<String>,
    pub deep_link: String,
    pub occurred_at_ms: i64,
}

/// One item in the shared ActivityKit content state.  Android clients may
/// consume the same record for an expanded notification, but the Host never
/// sends platform-specific view decisions through this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushActivityItem {
    pub environment_id: String,
    pub thread_id: String,
    pub project_title: String,
    pub thread_title: String,
    pub model_title: String,
    pub phase: PushActivityPhase,
    pub status: String,
    pub updated_at: String,
    pub deep_link: String,
}

/// The JSON shape consumed by the iOS Live Activity extension.  Field names
/// deliberately match the Swift Codable record and are covered by protocol
/// tests below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushContentState {
    pub title: String,
    pub subtitle: String,
    pub active_count: u32,
    pub updated_at: String,
    pub activities: Vec<PushActivityItem>,
}

impl PushActivityEvent {
    pub fn activity_record(&self) -> ActivityRecord {
        ActivityRecord {
            environment_id: self.host_id.clone(),
            thread_id: self.thread_id.clone(),
            project_title: self.project_title.clone(),
            thread_title: self.thread_title.clone(),
            model_title: self.model_title.clone(),
            phase: self.phase.wire_name().into(),
            headline: self.headline.clone(),
            updated_at_ms: self.occurred_at_ms,
            deep_link: self.deep_link.clone(),
        }
    }

    pub fn notification_enabled(&self, preferences: PushPreferences) -> bool {
        preferences.notifications_enabled
            && match self.phase {
                PushActivityPhase::WaitingForApproval => preferences.notify_on_approval,
                PushActivityPhase::WaitingForInput => preferences.notify_on_input,
                PushActivityPhase::Completed => preferences.notify_on_completion,
                PushActivityPhase::Failed => preferences.notify_on_failure,
                PushActivityPhase::Starting
                | PushActivityPhase::Running
                | PushActivityPhase::Stale => false,
            }
    }
}

impl From<ActivityContentState> for PushContentState {
    fn from(value: ActivityContentState) -> Self {
        Self {
            title: value.title,
            subtitle: value.subtitle,
            active_count: value.active_count,
            updated_at: value.updated_at,
            activities: value
                .activities
                .into_iter()
                .map(|item| PushActivityItem {
                    environment_id: item.environment_id,
                    thread_id: item.thread_id,
                    project_title: item.project_title,
                    thread_title: item.thread_title,
                    model_title: item.model_title,
                    phase: PushActivityPhase::from_wire(&item.phase),
                    status: item.status,
                    updated_at: item.updated_at,
                    deep_link: item.deep_link,
                })
                .collect(),
        }
    }
}

fn validate_text(value: &str, label: &str, max_bytes: usize) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed != value {
        return Err(format!("{label} cannot start or end with whitespace"));
    }
    let value = trimmed;
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    if value.len() > max_bytes {
        return Err(format!("{label} is too long"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} contains a control character"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ios() -> RegisterPushDevice {
        RegisterPushDevice {
            device_id: "device-1".into(),
            platform: PushPlatform::Ios,
            token: "token".into(),
            live_activity_token: Some("activity-token".into()),
            push_to_start_token: Some("start-token".into()),
            bundle_id: Some("dev.remoteagent.mobile".into()),
            apns_environment: Some(ApnsEnvironment::Sandbox),
            preferences: PushPreferences::default(),
        }
    }

    #[test]
    fn registration_rejects_cross_platform_fields() {
        let mut registration = ios();
        registration.platform = PushPlatform::Android;
        assert!(registration.validate().is_err());
        registration = ios();
        registration.apns_environment = None;
        assert!(registration.validate().is_err());
        registration = ios();
        registration.token = " token".into();
        assert!(registration.validate().is_err());
    }

    #[test]
    fn activity_preferences_are_per_event() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::WaitingForApproval,
            headline: "Approval needed".into(),
            detail: None,
            deep_link: "remoteagent://threads/host/thread".into(),
            occurred_at_ms: 1,
        };
        let mut preferences = PushPreferences::default();
        preferences.notify_on_approval = false;
        assert!(!event.notification_enabled(preferences));
        preferences.notify_on_approval = true;
        assert!(event.notification_enabled(preferences));
    }

    #[test]
    fn event_round_trips_without_provider_specific_fields() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Completed,
            headline: "Finished".into(),
            detail: Some("Done".into()),
            deep_link: "remoteagent://threads/host/thread".into(),
            occurred_at_ms: 123,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["phase"], "completed");
        assert_eq!(
            serde_json::from_value::<PushActivityEvent>(json).unwrap(),
            event
        );
    }

    #[test]
    fn activity_state_matches_native_codable_shape() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::WaitingForInput,
            headline: "Input needed".into(),
            detail: None,
            deep_link: "remoteagent://threads/host/thread".into(),
            occurred_at_ms: 123,
        };
        let state: PushContentState =
            agent_domain::activity_content_state(&[event.activity_record()]).into();
        let json = serde_json::to_value(state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "title": "Project",
                "subtitle": "Input needed",
                "activeCount": 1,
                "updatedAt": "1970-01-01T00:00:00.123Z",
                "activities": [{
                    "environmentId": "host",
                    "threadId": "thread",
                    "projectTitle": "Project",
                    "threadTitle": "Thread",
                    "modelTitle": "Model",
                    "phase": "waiting_for_input",
                    "status": "Input",
                    "updatedAt": "1970-01-01T00:00:00.123Z",
                    "deepLink": "remoteagent://threads/host/thread"
                }]
            })
        );
    }
}
