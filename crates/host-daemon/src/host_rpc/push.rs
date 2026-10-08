//! Direct operating-system push delivery owned by the Host.
//!
//! Device tokens are registered by an authenticated client and stored in a
//! private Host file. The conversation runtime produces one provider-neutral
//! awareness event; this module owns persistence, APNs/FCM translation,
//! bounded HTTP, retry policy and token invalidation.

use agent_domain::{
    ACTIVITY_ROWS_LIMIT, ACTIVITY_STATUS_LIMIT, ACTIVITY_SUMMARY_LIMIT, ActivityAlert,
    BackgroundKind, RunStatus, ThreadRelationship, activity_alert_for_transition,
    activity_expiry_at_ms, activity_expiry_is_due, bounded_activity_link, bounded_activity_text,
};
use agent_protocol::push::{
    ApnsEnvironment, PushActivityEvent, PushActivityPhase, PushContentState, PushPlatform,
    RegisterPushDevice, SetPushDeviceActive,
};
use agent_runtime::{HostProject, ShellSubscribe, ShellThread, ShellUpdate};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::StreamExt;
use ring::{digest, rand::SystemRandom, signature};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

const MAX_DEVICES: usize = 256;
const MAX_REGISTRY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_PROVIDER_PAYLOAD_BYTES: usize = 4 * 1024;
const MAX_FCM_ACTIVITY_BYTES: usize = 2_400;
const MAX_ATTEMPTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const ACTIVITY_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const PUSH_TO_START_RETRY_AFTER_MS: i64 = 10 * 60 * 1_000;
const APNS_JWT_CACHE_SECONDS: u64 = 45 * 60;
const APNS_PRODUCTION: &str = "https://api.push.apple.com";
const APNS_SANDBOX: &str = "https://api.sandbox.push.apple.com";
const FCM_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const FCM_AUDIENCE: &str = "https://oauth2.googleapis.com/token";
const FCM_ENDPOINT: &str = "https://fcm.googleapis.com/v1/projects";

/// A request deliberately omits Debug: its headers and body may contain
/// provider credentials or device tokens.
#[derive(Clone)]
struct HttpRequest {
    method: &'static str,
    url: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

#[async_trait]
trait PushTransport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, String>;
}

struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    fn new() -> Self {
        Self {
            client: crate::http::client(),
        }
    }
}

#[async_trait]
impl PushTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| "invalid push HTTP method".to_owned())?;
        let mut builder = self.client.request(method, &request.url).body(request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = tokio::time::timeout(REQUEST_TIMEOUT, builder.send())
            .await
            .map_err(|_| "push HTTP request timed out".to_owned())?
            .map_err(|_| "push HTTP request failed".to_owned())?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err("push HTTP response exceeded the size limit".to_owned());
        }
        let status = response.status().as_u16();
        let body = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| "push HTTP response failed".to_owned())?;
                if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err("push HTTP response exceeded the size limit".to_owned());
                }
                body.extend_from_slice(&chunk);
            }
            Ok::<_, String>(body)
        })
        .await
        .map_err(|_| "push HTTP response timed out".to_owned())??;
        Ok(HttpResponse { status, body })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredDevice {
    principal: String,
    registration: RegisterPushDevice,
    active: bool,
    activity_start_sent: Option<String>,
    activity_start_sent_at_ms: Option<i64>,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DeviceState {
    devices: BTreeMap<String, StoredDevice>,
}

/// Credentials are read only when a delivery is attempted. Values are never
/// included in errors or tracing fields, and this type intentionally has no
/// Debug implementation.
struct ProviderConfig {
    apns: Option<ApnsConfig>,
    fcm: Option<FcmConfig>,
}

struct ApnsConfig {
    team_id: String,
    key_id: String,
    private_key: String,
}

struct FcmConfig {
    project_id: String,
    client_email: String,
    private_key: String,
}

struct CachedProviderToken {
    value: String,
    expires_at: u64,
}

#[derive(Default)]
struct ProviderTokenCache {
    apns: Option<CachedProviderToken>,
    fcm: Option<CachedProviderToken>,
}

struct TokenMatch {
    device_id: String,
    token: String,
}

impl ProviderConfig {
    fn from_environment() -> Self {
        Self {
            apns: env_triplet("APNS_TEAM_ID", "APNS_KEY_ID", "APNS_PRIVATE_KEY").map(
                |(team_id, key_id, private_key)| ApnsConfig {
                    team_id,
                    key_id,
                    private_key,
                },
            ),
            fcm: env_triplet("FCM_PROJECT_ID", "FCM_CLIENT_EMAIL", "FCM_PRIVATE_KEY").map(
                |(project_id, client_email, private_key)| FcmConfig {
                    project_id,
                    client_email,
                    private_key,
                },
            ),
        }
    }
}

fn env_triplet(first: &str, second: &str, third: &str) -> Option<(String, String, String)> {
    let values = [first, second, third].map(|name| std::env::var(name).ok());
    match values {
        [Some(first), Some(second), Some(third)]
            if !first.trim().is_empty()
                && !second.trim().is_empty()
                && !third.trim().is_empty() =>
        {
            Some((first, second, third))
        }
        _ => None,
    }
}

/// Host-owned token registry and delivery service.
pub(crate) struct PushService {
    path: PathBuf,
    devices: Mutex<DeviceState>,
    transport: Arc<dyn PushTransport>,
    host_id: RwLock<String>,
    latest_state: RwLock<Option<PushContentState>>,
    provider_tokens: Mutex<ProviderTokenCache>,
    stop: CancellationToken,
    watcher: OnceLock<AbortOnDropHandle<()>>,
    /// The watcher assigns monotonically increasing sequence numbers. Holding
    /// this guard across provider I/O makes a slower older delivery unable to
    /// overwrite a newer aggregate state.
    delivery: Mutex<u64>,
    started_at_ms: i64,
}

impl PushService {
    pub(crate) fn new(path: PathBuf) -> anyhow::Result<Arc<Self>> {
        Self::with_transport(path, Arc::new(ReqwestTransport::new()))
    }

    fn with_transport(
        path: PathBuf,
        transport: Arc<dyn PushTransport>,
    ) -> anyhow::Result<Arc<Self>> {
        let state = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_REGISTRY_BYTES => {
                return Err(anyhow::anyhow!(
                    "saved push registrations exceed the size limit"
                ));
            }
            Ok(_) => {
                let bytes = std::fs::read(&path).map_err(|error| {
                    anyhow::anyhow!("saved push registrations could not be read: {error}")
                })?;
                if bytes.len() as u64 > MAX_REGISTRY_BYTES {
                    return Err(anyhow::anyhow!(
                        "saved push registrations exceed the size limit"
                    ));
                }
                serde_json::from_slice(&bytes).map_err(|error| {
                    anyhow::anyhow!("saved push registrations are invalid: {error}")
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DeviceState::default(),
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "saved push registrations could not be read: {error}"
                ));
            }
        };
        if state.devices.len() > MAX_DEVICES {
            return Err(anyhow::anyhow!(
                "saved push registrations exceed the device limit"
            ));
        }
        for (device_id, device) in &state.devices {
            if device_id != &device.registration.device_id {
                return Err(anyhow::anyhow!(
                    "saved push registration has a mismatched device id"
                ));
            }
            device
                .registration
                .validate()
                .map_err(|error| anyhow::anyhow!("saved push registration is invalid: {error}"))?;
            if device.activity_start_sent.is_some()
                && (device.registration.platform != PushPlatform::Ios
                    || device.registration.live_activity_token.is_some()
                    || device.registration.push_to_start_token.as_deref()
                        != device.activity_start_sent.as_deref())
            {
                return Err(anyhow::anyhow!(
                    "saved push registration has an invalid ActivityKit start marker"
                ));
            }
            if device.activity_start_sent.is_some() && device.activity_start_sent_at_ms.is_none() {
                return Err(anyhow::anyhow!(
                    "saved push registration is missing the ActivityKit start timestamp"
                ));
            }
        }
        Ok(Arc::new(Self {
            path,
            devices: Mutex::new(state),
            transport,
            host_id: RwLock::new("host".into()),
            latest_state: RwLock::new(None),
            provider_tokens: Mutex::new(ProviderTokenCache::default()),
            stop: CancellationToken::new(),
            watcher: OnceLock::new(),
            delivery: Mutex::new(0),
            started_at_ms: current_millis(),
        }))
    }

    pub(crate) fn set_host_id(&self, host_id: String) {
        if !host_id.trim().is_empty()
            && let Ok(mut value) = self.host_id.try_write()
        {
            *value = host_id;
        }
    }

    pub(crate) async fn register(
        &self,
        principal: &str,
        registration: RegisterPushDevice,
    ) -> anyhow::Result<()> {
        registration
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        ensure_principal(principal)?;
        let mut state = self.devices.lock().await;
        if let Some(existing) = state.devices.get(&registration.device_id)
            && existing.principal != principal
        {
            return Err(anyhow::anyhow!("push device belongs to another connection"));
        }
        if !state.devices.contains_key(&registration.device_id)
            && state.devices.len() >= MAX_DEVICES
        {
            return Err(anyhow::anyhow!("push device limit reached"));
        }
        let previous_start = state
            .devices
            .get(&registration.device_id)
            .and_then(|device| {
                (device.registration.platform == PushPlatform::Ios
                    && device.registration.push_to_start_token == registration.push_to_start_token
                    && registration.live_activity_token.is_none())
                .then(|| {
                    (
                        device.activity_start_sent.clone(),
                        device.activity_start_sent_at_ms,
                    )
                })
            });
        let replay_registration = registration.clone();
        let mut next = state.clone();
        // Registration and activation are separate RPCs. A requested delivery
        // starts active so a successful registration cannot lose the first
        // event while the native client waits for the follow-up active RPC;
        // disabled registrations remain inactive until preferences opt in.
        let wants_delivery = registration.preferences.notifications_enabled
            || registration.preferences.live_activities_enabled;
        let active = wants_delivery;
        next.devices.insert(
            registration.device_id.clone(),
            StoredDevice {
                principal: principal.into(),
                registration,
                active,
                activity_start_sent: previous_start.as_ref().and_then(|value| value.0.clone()),
                activity_start_sent_at_ms: previous_start.and_then(|value| value.1),
            },
        );
        self.persist(&next).await?;
        *state = next;
        drop(state);
        if active {
            self.replay_activity(&replay_registration).await;
        }
        Ok(())
    }

    pub(crate) async fn unregister(&self, principal: &str, device_id: &str) -> anyhow::Result<()> {
        ensure_principal(principal)?;
        let mut state = self.devices.lock().await;
        if let Some(existing) = state.devices.get(device_id)
            && existing.principal != principal
        {
            return Err(anyhow::anyhow!("push device belongs to another connection"));
        }
        if !state.devices.contains_key(device_id) {
            return Ok(());
        }
        let mut next = state.clone();
        next.devices.remove(device_id);
        self.persist(&next).await?;
        *state = next;
        Ok(())
    }

    /// Revocation is stronger than a connection closing: remove every token
    /// owned by the revoked authenticated principal before its connection is
    /// torn down. Ordinary disconnects intentionally retain registrations so
    /// the Host can continue delivering while the app is backgrounded.
    pub(crate) async fn remove_principal(&self, principal: &str) -> anyhow::Result<()> {
        ensure_principal(principal)?;
        let mut state = self.devices.lock().await;
        let mut next = state.clone();
        next.devices
            .retain(|_, device| device.principal != principal);
        if next.devices == state.devices {
            return Ok(());
        }
        self.persist(&next).await?;
        *state = next;
        Ok(())
    }

    pub(crate) async fn set_active(
        &self,
        principal: &str,
        params: &SetPushDeviceActive,
    ) -> anyhow::Result<()> {
        params.validate().map_err(|error| anyhow::anyhow!(error))?;
        ensure_principal(principal)?;
        let mut state = self.devices.lock().await;
        let Some(existing) = state.devices.get(&params.device_id) else {
            return Err(anyhow::anyhow!("push device is not registered"));
        };
        if existing.principal != principal {
            return Err(anyhow::anyhow!("push device belongs to another connection"));
        }
        let was_active = existing.active;
        let mut next = state.clone();
        next.devices
            .get_mut(&params.device_id)
            .expect("device checked above")
            .active = params.active;
        self.persist(&next).await?;
        *state = next;
        let replay_registration = if params.active && !was_active {
            state
                .devices
                .get(&params.device_id)
                .map(|device| device.registration.clone())
        } else {
            None
        };
        drop(state);
        if let Some(replay_registration) = replay_registration {
            self.replay_activity(&replay_registration).await;
        }
        Ok(())
    }

    async fn persist(&self, state: &DeviceState) -> anyhow::Result<()> {
        let path = self.path.clone();
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let bytes = serde_json::to_vec(&state)?;
            if bytes.len() as u64 > MAX_REGISTRY_BYTES {
                return Err(anyhow::anyhow!(
                    "push registration state exceeds the size limit"
                ));
            }
            crate::platform::save_private_bytes(&path, &bytes)
        })
        .await
        .map_err(|error| anyhow::anyhow!("push registration write task failed: {error}"))??;
        Ok(())
    }

    /// Starts one shell watcher. The first snapshot seeds awareness without
    /// sending historical notifications; later transitions are delivered in
    /// the background so shell publication is never held on provider I/O.
    pub(crate) fn start(self: &Arc<Self>, runtime: Arc<agent_runtime::Runtime>) {
        if self.watcher.get().is_some() {
            return;
        }
        let service = Arc::downgrade(self);
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            run_watcher(service, runtime).await;
        }));
        let _ = self.watcher.set(task);
    }

    pub(crate) fn shutdown(&self) {
        self.stop.cancel();
    }

    async fn deliver(
        &self,
        sequence: u64,
        event: Option<PushActivityEvent>,
        active: Vec<PushActivityEvent>,
        previous: Vec<PushActivityEvent>,
        previous_available: bool,
    ) {
        // Shell updates can arrive faster than a provider round trip. Keep
        // the provider order equal to the semantic event order so a delayed
        // request cannot overwrite a newer ActivityKit state.
        let mut last_sequence = self.delivery.lock().await;
        if sequence <= *last_sequence {
            return;
        }
        *last_sequence = sequence;
        let metadata_event = event.as_ref().or_else(|| previous.first());
        let Some(metadata_event) = metadata_event else {
            return;
        };
        let content_state = event
            .as_ref()
            .map_or_else(empty_content_state, |event| content_state(event, &active));
        let activity_expires_at_ms = event
            .as_ref()
            .map_or(0, |event| content_state_expiry_at_ms(&content_state, event));
        let clear_source_at_ms = event
            .is_none()
            .then(|| clear_source_timestamp(&previous).unwrap_or(metadata_event.occurred_at_ms));
        let delivery_at_ms = current_millis();
        let previous_records = previous
            .iter()
            .map(PushActivityEvent::activity_record)
            .collect::<Vec<_>>();
        let next_records = active
            .iter()
            .map(PushActivityEvent::activity_record)
            .collect::<Vec<_>>();
        *self.latest_state.write().await = (!content_state.activities.is_empty()
            || content_state.active_count > 0)
            .then_some(content_state.clone());
        let records = {
            let state = self.devices.lock().await;
            state
                .devices
                .values()
                .filter(|device| device.active)
                .cloned()
                .collect::<Vec<_>>()
        };
        if records.is_empty() {
            return;
        }
        let config = ProviderConfig::from_environment();
        let mut invalid_normal = Vec::<TokenMatch>::new();
        let mut invalid_activity = Vec::<TokenMatch>::new();
        let mut invalid_start = Vec::<TokenMatch>::new();
        let mut ended_activity = Vec::<TokenMatch>::new();
        for device in records {
            let device_id = device.registration.device_id.clone();
            let should_deliver_activity = device.registration.platform == PushPlatform::Android
                && device.registration.preferences.live_activities_enabled;
            let alert = activity_alert_for_transition(
                &previous_records,
                &next_records,
                previous_available,
                delivery_at_ms,
                device.registration.preferences.notifications_enabled,
                device.registration.preferences.notify_on_approval,
                device.registration.preferences.notify_on_input,
                device.registration.preferences.notify_on_completion,
                device.registration.preferences.notify_on_failure,
            );
            if (alert.is_some() || should_deliver_activity)
                && let Err(result) = self
                    .send_notification(
                        &config,
                        &device.registration,
                        event.as_ref(),
                        &metadata_event.host_id,
                        &content_state,
                        activity_expires_at_ms,
                        delivery_at_ms,
                        clear_source_at_ms,
                        alert.as_ref(),
                    )
                    .await
                && result == DeliveryError::InvalidToken
            {
                invalid_normal.push(TokenMatch {
                    device_id: device_id.clone(),
                    token: device.registration.token.clone(),
                });
            }
            if device.registration.preferences.live_activities_enabled
                && device.registration.platform == PushPlatform::Ios
                && let Some((activity_token, activity_event)) = live_activity_target_for_device(
                    &device,
                    &content_state,
                    event
                        .as_ref()
                        .map_or(PushActivityPhase::Completed, |event| event.phase),
                )
            {
                let had_activity_token = device.registration.live_activity_token.is_some();
                match self
                    .send_live_activity(
                        &config,
                        &device.registration,
                        &content_state,
                        activity_token,
                        activity_event,
                    )
                    .await
                {
                    Ok(()) if activity_event == "start" => {
                        self.mark_activity_start(&device_id, activity_token).await;
                    }
                    Ok(()) if activity_event == "end" && had_activity_token => {
                        ended_activity.push(TokenMatch {
                            device_id: device_id.clone(),
                            token: activity_token.to_owned(),
                        });
                    }
                    Err(DeliveryError::InvalidToken) if had_activity_token => {
                        invalid_activity.push(TokenMatch {
                            device_id: device_id.clone(),
                            token: activity_token.to_owned(),
                        });
                    }
                    Err(DeliveryError::InvalidToken) => {
                        invalid_start.push(TokenMatch {
                            device_id: device_id.clone(),
                            token: activity_token.to_owned(),
                        });
                    }
                    _ => {}
                }
            }
        }
        self.invalidate(
            invalid_normal,
            invalid_activity,
            invalid_start,
            ended_activity,
        )
        .await;
    }

    /// A token can arrive after work has already started (or after the app
    /// was relaunched). Repaint the current aggregate once, without sending a
    /// notification transition or remotely starting an idle card.
    async fn replay_activity(&self, registration: &RegisterPushDevice) {
        // Take the delivery gate before reading the aggregate. Otherwise a
        // watcher update can publish a newer state while this replay still
        // holds an older snapshot, and the late registration repaint would
        // move the ActivityKit card backwards.
        let _delivery = self.delivery.lock().await;
        let Some(state) = self.latest_state.read().await.clone() else {
            return;
        };
        let device = {
            let devices = self.devices.lock().await;
            devices.devices.get(&registration.device_id).cloned()
        };
        let Some(device) = device else { return };
        if device.registration.platform != PushPlatform::Ios
            || !device.registration.preferences.live_activities_enabled
        {
            return;
        }
        let phase = if state.active_count == 0 {
            PushActivityPhase::Completed
        } else {
            PushActivityPhase::Running
        };
        let Some((token, event)) = live_activity_target_for_device(&device, &state, phase) else {
            return;
        };
        let config = ProviderConfig::from_environment();
        match self
            .send_live_activity(&config, &device.registration, &state, token, event)
            .await
        {
            Ok(()) if event == "start" => {
                self.mark_activity_start(&device.registration.device_id, token)
                    .await;
            }
            Err(DeliveryError::InvalidToken) => {
                let mut activity = Vec::new();
                let mut start = Vec::new();
                if device.registration.live_activity_token.as_deref() == Some(token) {
                    activity.push(TokenMatch {
                        device_id: device.registration.device_id.clone(),
                        token: token.to_owned(),
                    });
                } else {
                    start.push(TokenMatch {
                        device_id: device.registration.device_id.clone(),
                        token: token.to_owned(),
                    });
                }
                drop(_delivery);
                self.invalidate(Vec::new(), activity, start, Vec::new())
                    .await;
            }
            _ => {}
        }
    }

    async fn invalidate(
        &self,
        normal: Vec<TokenMatch>,
        activity: Vec<TokenMatch>,
        start: Vec<TokenMatch>,
        ended: Vec<TokenMatch>,
    ) {
        if normal.is_empty() && activity.is_empty() && start.is_empty() && ended.is_empty() {
            return;
        }
        let mut state = self.devices.lock().await;
        let mut next = state.clone();
        for item in normal {
            if next
                .devices
                .get(&item.device_id)
                .is_some_and(|device| device.registration.token == item.token)
            {
                next.devices.remove(&item.device_id);
            }
        }
        for item in activity {
            if let Some(device) = next.devices.get_mut(&item.device_id)
                && device.registration.live_activity_token.as_deref() == Some(item.token.as_str())
            {
                device.registration.live_activity_token = None;
                device.activity_start_sent = None;
                device.activity_start_sent_at_ms = None;
            }
        }
        for item in start {
            if let Some(device) = next.devices.get_mut(&item.device_id)
                && device.registration.push_to_start_token.as_deref() == Some(item.token.as_str())
            {
                device.registration.push_to_start_token = None;
                device.activity_start_sent = None;
                device.activity_start_sent_at_ms = None;
            }
        }
        for item in ended {
            if let Some(device) = next.devices.get_mut(&item.device_id)
                && device.registration.live_activity_token.as_deref() == Some(item.token.as_str())
            {
                device.registration.live_activity_token = None;
                device.activity_start_sent = None;
                device.activity_start_sent_at_ms = None;
            }
        }
        if next.devices == state.devices {
            return;
        }
        if let Err(error) = self.persist(&next).await {
            tracing::warn!(target = "push", operation = "registration_invalidation", message = %error);
            return;
        }
        *state = next;
    }

    async fn mark_activity_start(&self, device_id: &str, token: &str) {
        let mut state = self.devices.lock().await;
        let Some(device) = state.devices.get(device_id) else {
            return;
        };
        if device.registration.push_to_start_token.as_deref() != Some(token)
            || device.registration.live_activity_token.is_some()
        {
            return;
        }
        let mut next = state.clone();
        let Some(device) = next.devices.get_mut(device_id) else {
            return;
        };
        device.activity_start_sent = Some(token.to_owned());
        device.activity_start_sent_at_ms = Some(current_millis());
        if let Err(error) = self.persist(&next).await {
            tracing::warn!(target = "push", operation = "activity_start_marker", message = %error);
            return;
        }
        *state = next;
    }

    /// Sends one provider request with independently sourced registration,
    /// activity, expiry, delivery-clock, and alert facts. The flat call keeps
    /// provider payload decisions explicit without a mutable request wrapper.
    #[allow(clippy::too_many_arguments)]
    async fn send_notification(
        &self,
        config: &ProviderConfig,
        registration: &RegisterPushDevice,
        event: Option<&PushActivityEvent>,
        host_id: &str,
        state: &PushContentState,
        activity_expires_at_ms: i64,
        delivery_at_ms: i64,
        clear_source_at_ms: Option<i64>,
        alert: Option<&ActivityAlert>,
    ) -> Result<(), DeliveryError> {
        let request = match registration.platform {
            PushPlatform::Ios => {
                let jwt = self
                    .apns_provider_token(config.apns.as_ref().ok_or(DeliveryError::Unavailable)?)
                    .await?;
                apns_notification_request(
                    registration,
                    event.ok_or(DeliveryError::Unavailable)?,
                    alert.ok_or(DeliveryError::Unavailable)?,
                    jwt,
                )?
            }
            PushPlatform::Android => {
                let project_id = config
                    .fcm
                    .as_ref()
                    .ok_or(DeliveryError::Unavailable)?
                    .project_id
                    .clone();
                let access_token = self
                    .fcm_provider_token(config.fcm.as_ref().ok_or(DeliveryError::Unavailable)?)
                    .await?;
                fcm_notification_request(
                    &project_id,
                    registration,
                    event,
                    host_id,
                    state,
                    activity_expires_at_ms,
                    access_token,
                    delivery_at_ms,
                    clear_source_at_ms,
                    alert,
                )?
            }
        };
        self.send_request(request).await
    }

    async fn send_live_activity(
        &self,
        config: &ProviderConfig,
        registration: &RegisterPushDevice,
        state: &PushContentState,
        token: &str,
        event: &str,
    ) -> Result<(), DeliveryError> {
        let jwt = self
            .apns_provider_token(config.apns.as_ref().ok_or(DeliveryError::Unavailable)?)
            .await?;
        let request = apns_live_activity_request(registration, state, token, event, jwt)?;
        self.send_request(request).await
    }

    async fn apns_provider_token(&self, config: &ApnsConfig) -> Result<String, DeliveryError> {
        let now = unix_seconds();
        let cached = {
            let cache = self.provider_tokens.lock().await;
            cache
                .apns
                .as_ref()
                .filter(|token| token.expires_at > now)
                .map(|token| token.value.clone())
        };
        if let Some(token) = cached {
            return Ok(token);
        }
        let value = apns_jwt(config)?;
        let mut cache = self.provider_tokens.lock().await;
        if let Some(token) = cache
            .apns
            .as_ref()
            .filter(|token| token.expires_at > now)
            .map(|token| token.value.clone())
        {
            return Ok(token);
        }
        cache.apns = Some(CachedProviderToken {
            value: value.clone(),
            expires_at: now.saturating_add(APNS_JWT_CACHE_SECONDS),
        });
        Ok(value)
    }

    async fn fcm_provider_token(&self, config: &FcmConfig) -> Result<String, DeliveryError> {
        if !valid_project_id(&config.project_id) {
            return Err(DeliveryError::Unavailable);
        }
        let now = unix_seconds();
        let cached = {
            let cache = self.provider_tokens.lock().await;
            cache
                .fcm
                .as_ref()
                .filter(|token| token.expires_at > now)
                .map(|token| token.value.clone())
        };
        if let Some(token) = cached {
            return Ok(token);
        }
        let (value, expires_in) =
            fcm_access_token(config, self.transport.clone(), self.stop.clone()).await?;
        let mut cache = self.provider_tokens.lock().await;
        if let Some(token) = cache
            .fcm
            .as_ref()
            .filter(|token| token.expires_at > now)
            .map(|token| token.value.clone())
        {
            return Ok(token);
        }
        cache.fcm = Some(CachedProviderToken {
            value: value.clone(),
            expires_at: now.saturating_add(expires_in.saturating_sub(60).max(60)),
        });
        Ok(value)
    }

    async fn send_request(&self, request: HttpRequest) -> Result<(), DeliveryError> {
        for attempt in 0..MAX_ATTEMPTS {
            let response = tokio::select! {
                _ = self.stop.cancelled() => return Err(DeliveryError::Cancelled),
                response = self.transport.send(request.clone()) => {
                    match response {
                        Ok(response) => response,
                        Err(_) if attempt + 1 < MAX_ATTEMPTS => {
                            tokio::select! {
                                _ = self.stop.cancelled() => return Err(DeliveryError::Cancelled),
                                _ = tokio::time::sleep(retry_delay(attempt)) => {}
                            }
                            continue;
                        }
                        Err(_) => return Err(DeliveryError::Transport),
                    }
                }
            };
            if (200..300).contains(&response.status) {
                return Ok(());
            }
            if invalid_token_response(response.status, &response.body) {
                return Err(DeliveryError::InvalidToken);
            }
            if matches!(response.status, 401 | 403) {
                // Provider credentials may rotate while the Host remains up.
                // Drop cached bearer material before the next delivery without
                // exposing the response body or credential values.
                let mut cache = self.provider_tokens.lock().await;
                cache.apns = None;
                cache.fcm = None;
            }
            if !retryable_status(response.status) || attempt + 1 == MAX_ATTEMPTS {
                return Err(DeliveryError::Provider);
            }
            tokio::select! {
                _ = self.stop.cancelled() => return Err(DeliveryError::Cancelled),
                _ = tokio::time::sleep(retry_delay(attempt)) => {}
            }
        }
        Err(DeliveryError::Provider)
    }
}

fn live_activity_target<'a>(
    registration: &'a RegisterPushDevice,
    state: &PushContentState,
    phase: PushActivityPhase,
) -> Option<(&'a str, &'static str)> {
    if let Some(token) = registration.live_activity_token.as_deref() {
        return Some((
            token,
            if phase.is_terminal() && state.active_count == 0 {
                "end"
            } else {
                "update"
            },
        ));
    }
    (state.active_count > 0)
        .then_some(registration.push_to_start_token.as_deref())
        .flatten()
        .map(|token| (token, "start"))
}

fn live_activity_target_for_device<'a>(
    device: &'a StoredDevice,
    state: &PushContentState,
    phase: PushActivityPhase,
) -> Option<(&'a str, &'static str)> {
    if device.registration.live_activity_token.is_none()
        && device.activity_start_sent.as_deref()
            == device.registration.push_to_start_token.as_deref()
        && device.activity_start_sent_at_ms.is_some_and(|sent| {
            current_millis().saturating_sub(sent) < PUSH_TO_START_RETRY_AFTER_MS
        })
    {
        return None;
    }
    live_activity_target(&device.registration, state, phase)
}

impl Drop for PushService {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn ensure_principal(principal: &str) -> anyhow::Result<()> {
    if principal.trim().is_empty() {
        Err(anyhow::anyhow!("authenticated connection is required"))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryError {
    InvalidToken,
    Unavailable,
    Transport,
    Provider,
    Cancelled,
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(250 * 2u64.saturating_pow(attempt.min(3) as u32))
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 425 || status == 429 || (500..600).contains(&status)
}

fn invalid_token_response(status: u16, body: &[u8]) -> bool {
    if status == 410 {
        return true;
    }
    if !(status == 400 || status == 404) {
        return false;
    }
    if serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .is_some_and(|value| json_has_unregistered_code(&value))
    {
        return true;
    }
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    if body.contains("baddevicetoken")
        || body.contains("devicetokennottopic")
        || body.contains("registration-token-not-registered")
    {
        return true;
    }
    false
}

fn json_has_unregistered_code(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            (key == "errorCode" || key == "status")
                && value.as_str().is_some_and(|code| code == "UNREGISTERED")
                || json_has_unregistered_code(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(json_has_unregistered_code),
        _ => false,
    }
}

async fn run_watcher(service: std::sync::Weak<PushService>, runtime: Arc<agent_runtime::Runtime>) {
    let mut active = HashMap::<String, PushActivityEvent>::new();
    let mut terminal = HashMap::<String, PushActivityEvent>::new();
    let mut last_seen = HashMap::<String, PushActivityEvent>::new();
    let mut projects = HashMap::<String, HostProject>::new();
    let mut delivery_sequence = 0_u64;
    loop {
        let Some(service) = service.upgrade() else {
            return;
        };
        let mut subscription = match runtime.subscribe_shell(ShellSubscribe::default()).await {
            Ok(subscription) => subscription,
            Err(error) => {
                tracing::warn!(target = "push", operation = "shell_subscribe", message = %error);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let mut seeded = false;
        let mut sweep = tokio::time::interval(ACTIVITY_SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let update = tokio::select! {
                _ = service.stop.cancelled() => return,
                _ = sweep.tick() => {
                    let now = current_millis();
                    let previous = current_activity_events(&active, &terminal);
                    let expired = active
                        .iter()
                        .filter(|(_, event)| activity_expired(event, now))
                        .map(|(thread_id, _)| thread_id.clone())
                        .collect::<Vec<_>>();
                    let terminal_count = terminal.len();
                    terminal.retain(|_, event| !activity_expired(event, now));
                    let terminal_pruned = terminal.len() != terminal_count;
                    for thread_id in expired {
                        delivery_sequence = delivery_sequence.saturating_add(1);
                        stale_removed_thread(
                            &service,
                            &mut active,
                            &mut terminal,
                            &mut last_seen,
                            &thread_id,
                            delivery_sequence,
                        );
                    }
                    if terminal_pruned {
                        let current = current_activity_events(&active, &terminal);
                        let event = current.first().cloned();
                        if event.is_some() || !previous.is_empty() {
                            delivery_sequence = delivery_sequence.saturating_add(1);
                            let sequence = delivery_sequence;
                            let service = service.clone();
                            tokio::spawn(async move {
                                service
                                    .deliver(sequence, event, current, previous, true)
                                    .await
                            });
                        }
                    }
                    continue;
                }
                update = subscription.updates.recv() => update,
            };
            let Some(update) = update else {
                break;
            };
            match update {
                ShellUpdate::Snapshot(snapshot) => {
                    projects = snapshot
                        .projects
                        .into_iter()
                        .map(|project| (project.id.clone(), project))
                        .collect();
                    active.clear();
                    terminal.clear();
                    last_seen.clear();
                    *service.latest_state.write().await = None;
                    let host_id = service.host_id.read().await.clone();
                    let now = current_millis();
                    for thread in snapshot.threads {
                        if let Some(event) = event_from_thread(&host_id, &projects, &thread) {
                            if is_live_phase(event.phase) && !activity_expired(&event, now) {
                                active.insert(event.thread_id.clone(), event.clone());
                            } else if event.phase.is_terminal() && !activity_expired(&event, now) {
                                terminal.insert(event.thread_id.clone(), event.clone());
                            }
                            last_seen.insert(event.thread_id.clone(), event);
                        }
                    }
                    let seeded_events = current_activity_events(&active, &terminal);
                    if let Some(seed) = seeded_events.first() {
                        *service.latest_state.write().await =
                            Some(content_state(seed, &seeded_events));
                    }
                    seeded = true;
                }
                ShellUpdate::Projects {
                    projects: values, ..
                } => {
                    projects = values
                        .into_iter()
                        .map(|project| (project.id.clone(), project))
                        .collect();
                }
                ShellUpdate::ProjectUpdated { project, .. } => {
                    projects.insert(project.id.clone(), project);
                }
                ShellUpdate::ProjectRemoved { project, .. } => {
                    projects.remove(&project);
                    let ids = last_seen
                        .values()
                        .filter(|event| event.project_id == project)
                        .map(|event| event.thread_id.clone())
                        .collect::<Vec<_>>();
                    for id in ids {
                        delivery_sequence = delivery_sequence.saturating_add(1);
                        stale_removed_thread(
                            &service,
                            &mut active,
                            &mut terminal,
                            &mut last_seen,
                            &id,
                            delivery_sequence,
                        );
                    }
                }
                ShellUpdate::ThreadRemoved { thread, .. } => {
                    delivery_sequence = delivery_sequence.saturating_add(1);
                    stale_removed_thread(
                        &service,
                        &mut active,
                        &mut terminal,
                        &mut last_seen,
                        thread.as_str(),
                        delivery_sequence,
                    );
                }
                ShellUpdate::ThreadUpdated { thread, .. } if seeded => {
                    let host_id = service.host_id.read().await.clone();
                    let Some(event) = event_from_thread(&host_id, &projects, &thread) else {
                        delivery_sequence = delivery_sequence.saturating_add(1);
                        stale_removed_thread(
                            &service,
                            &mut active,
                            &mut terminal,
                            &mut last_seen,
                            thread.thread.as_str(),
                            delivery_sequence,
                        );
                        continue;
                    };
                    if event.phase.is_terminal()
                        && !last_seen.contains_key(&event.thread_id)
                        && event.occurred_at_ms <= service.started_at_ms
                    {
                        last_seen.insert(event.thread_id.clone(), event);
                        continue;
                    }
                    let changed = last_seen
                        .get(&event.thread_id)
                        .is_none_or(|previous| event_changed(previous, &event));
                    if !changed {
                        continue;
                    }
                    last_seen.insert(event.thread_id.clone(), event.clone());
                    let now = current_millis();
                    let previous = current_activity_events(&active, &terminal);
                    let mut current = previous.clone();
                    current.retain(|value| value.thread_id != event.thread_id);
                    if !activity_expired(&event, now) {
                        current.push(event.clone());
                    }
                    if is_live_phase(event.phase) && !activity_expired(&event, now) {
                        active.insert(event.thread_id.clone(), event.clone());
                        terminal.remove(&event.thread_id);
                    } else if event.phase.is_terminal() && !activity_expired(&event, now) {
                        terminal.insert(event.thread_id.clone(), event.clone());
                        active.remove(&event.thread_id);
                    } else {
                        active.remove(&event.thread_id);
                        terminal.remove(&event.thread_id);
                    }
                    delivery_sequence = delivery_sequence.saturating_add(1);
                    let sequence = delivery_sequence;
                    let service = service.clone();
                    tokio::spawn(async move {
                        service
                            .deliver(sequence, Some(event), current, previous, true)
                            .await
                    });
                }
                ShellUpdate::Synchronized | ShellUpdate::ThreadUpdated { .. } => {}
            }
        }
        if !subscription.updates.overflowed() {
            return;
        }
    }
}

fn is_live_phase(phase: PushActivityPhase) -> bool {
    matches!(
        phase,
        PushActivityPhase::Starting
            | PushActivityPhase::Running
            | PushActivityPhase::WaitingForApproval
            | PushActivityPhase::WaitingForInput
    )
}

fn activity_expired(event: &PushActivityEvent, now_ms: i64) -> bool {
    activity_expiry_is_due(
        activity_expiry_at_ms(event.phase.wire_name(), event.occurred_at_ms),
        now_ms,
    )
}

fn current_activity_events(
    active: &HashMap<String, PushActivityEvent>,
    terminal: &HashMap<String, PushActivityEvent>,
) -> Vec<PushActivityEvent> {
    active.values().chain(terminal.values()).cloned().collect()
}

fn event_changed(previous: &PushActivityEvent, next: &PushActivityEvent) -> bool {
    previous.phase != next.phase
        || previous.headline != next.headline
        || previous.detail != next.detail
        || previous.project_title != next.project_title
        || previous.thread_title != next.thread_title
        || previous.model_title != next.model_title
        || previous.deep_link != next.deep_link
        || previous.occurred_at_ms != next.occurred_at_ms
}

fn event_from_thread(
    host_id: &str,
    projects: &HashMap<String, HostProject>,
    thread: &ShellThread,
) -> Option<PushActivityEvent> {
    let row = &thread.row.summary;
    if row.relationship_to_parent == Some(ThreadRelationship::Subagent)
        || row.archived_at.is_some()
        || row.deleted_at.is_some()
    {
        return None;
    }
    let project = projects.get(&row.project)?;
    let phase = phase_for_thread(row)?;
    let project_title = bounded_activity_text(&project.name, ACTIVITY_SUMMARY_LIMIT);
    let thread_title = if row.title.trim().is_empty() {
        "Untitled thread".into()
    } else {
        bounded_activity_text(&row.title, ACTIVITY_SUMMARY_LIMIT)
    };
    let headline = match phase {
        PushActivityPhase::WaitingForApproval => "Approval needed",
        PushActivityPhase::WaitingForInput => "Waiting for input",
        PushActivityPhase::Completed => "Agent finished",
        PushActivityPhase::Failed => "Agent failed",
        PushActivityPhase::Starting => "Starting agent",
        PushActivityPhase::Running => "Agent is working",
        PushActivityPhase::Stale => "Update delayed",
    }
    .into();
    let detail = match phase {
        PushActivityPhase::Completed => Some("Review the completed task.".into()),
        PushActivityPhase::Failed => Some("The agent run failed.".into()),
        PushActivityPhase::WaitingForApproval
        | PushActivityPhase::WaitingForInput
        | PushActivityPhase::Starting
        | PushActivityPhase::Running
        | PushActivityPhase::Stale => None,
    };
    let thread_id = thread.thread.to_string();
    Some(PushActivityEvent {
        host_id: host_id.into(),
        thread_id: thread_id.clone(),
        project_id: row.project.clone(),
        project_title,
        thread_title,
        model_title: bounded_activity_text(&row.selection.model, ACTIVITY_SUMMARY_LIMIT),
        phase,
        headline,
        detail,
        deep_link: thread_deep_link(host_id, &thread_id),
        occurred_at_ms: row.updated_at.millis(),
    })
}

fn phase_for_thread(row: &agent_domain::ThreadShell) -> Option<PushActivityPhase> {
    if let Some(request) = &row.pending_request
        && request.kind != "auth_refresh"
    {
        if request.kind == "user_input" {
            return Some(PushActivityPhase::WaitingForInput);
        }
        return Some(PushActivityPhase::WaitingForApproval);
    }
    match row.activity_run_status.or(row.status) {
        Some(RunStatus::Preparing | RunStatus::Starting) => Some(PushActivityPhase::Starting),
        Some(RunStatus::Running | RunStatus::Waiting) => Some(PushActivityPhase::Running),
        Some(RunStatus::Completed)
            if row
                .pending_background_work
                .iter()
                .any(|work| work.kind != BackgroundKind::Command) =>
        {
            Some(PushActivityPhase::Running)
        }
        Some(RunStatus::Completed) => Some(PushActivityPhase::Completed),
        Some(RunStatus::Failed) => Some(PushActivityPhase::Failed),
        Some(
            RunStatus::Queued
            | RunStatus::Interrupted
            | RunStatus::Cancelled
            | RunStatus::RolledBack,
        )
        | None => None,
    }
}

fn stale_removed_thread(
    service: &Arc<PushService>,
    active: &mut HashMap<String, PushActivityEvent>,
    terminal: &mut HashMap<String, PushActivityEvent>,
    last_seen: &mut HashMap<String, PushActivityEvent>,
    thread_id: &str,
    sequence: u64,
) {
    let previous_events = current_activity_events(active, terminal);
    let was_active = active.remove(thread_id).is_some();
    terminal.remove(thread_id);
    let Some(previous) = last_seen.remove(thread_id) else {
        return;
    };
    if !was_active || !is_live_phase(previous.phase) {
        return;
    }
    let event = PushActivityEvent {
        phase: PushActivityPhase::Stale,
        headline: "Update delayed".into(),
        detail: None,
        occurred_at_ms: previous.occurred_at_ms,
        ..previous
    };
    let mut current = current_activity_events(active, terminal);
    current.push(event.clone());
    let service = Arc::clone(service);
    tokio::spawn(async move {
        service
            .deliver(sequence, Some(event), current, previous_events, true)
            .await
    });
}

fn thread_deep_link(host_id: &str, thread_id: &str) -> String {
    format!(
        "remoteagent://threads/{}/{}",
        encode_component(host_id),
        encode_component(thread_id)
    )
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
            encoded.push(char::from(b"0123456789ABCDEF"[(byte & 0xf) as usize]));
        }
    }
    encoded
}

fn content_state(event: &PushActivityEvent, active: &[PushActivityEvent]) -> PushContentState {
    let mut records = active
        .iter()
        .map(PushActivityEvent::activity_record)
        .collect::<Vec<_>>();
    if !records
        .iter()
        .any(|value| value.environment_id == event.host_id && value.thread_id == event.thread_id)
    {
        records.push(event.activity_record());
    }
    agent_domain::activity_content_state(&records).into()
}

fn empty_content_state() -> PushContentState {
    PushContentState {
        title: "Agent activity".into(),
        subtitle: String::new(),
        active_count: 0,
        updated_at: String::new(),
        activities: Vec::new(),
    }
}

fn clear_source_timestamp(previous: &[PushActivityEvent]) -> Option<i64> {
    previous.iter().map(|event| event.occurred_at_ms).max()
}

fn content_state_expiry_at_ms(state: &PushContentState, event: &PushActivityEvent) -> i64 {
    state
        .activities
        .iter()
        .filter_map(|value| {
            let updated_at_ms = agent_domain::activity_timestamp_millis(&value.updated_at)?;
            Some(activity_expiry_at_ms(
                value.phase.wire_name(),
                updated_at_ms,
            ))
        })
        .max()
        .unwrap_or_else(|| activity_expiry_at_ms(event.phase.wire_name(), event.occurred_at_ms))
}

fn timestamp(millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .unwrap_or_else(|| DateTime::<Utc>::from(SystemTime::now()))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn current_millis() -> i64 {
    DateTime::<Utc>::from(SystemTime::now()).timestamp_millis()
}

fn apns_notification_request(
    registration: &RegisterPushDevice,
    event: &PushActivityEvent,
    alert: &ActivityAlert,
    jwt: String,
) -> Result<HttpRequest, DeliveryError> {
    let bundle_id = registration
        .bundle_id
        .as_deref()
        .ok_or(DeliveryError::Unavailable)?;
    let base = match registration.apns_environment {
        Some(ApnsEnvironment::Sandbox) => APNS_SANDBOX,
        Some(ApnsEnvironment::Production) => APNS_PRODUCTION,
        None => return Err(DeliveryError::Unavailable),
    };
    let url = apns_device_url(base, &registration.token)?;
    let headline = bounded_activity_text(&event.headline, ACTIVITY_SUMMARY_LIMIT);
    let detail = bounded_activity_text(
        event.detail.as_deref().unwrap_or(&event.thread_title),
        ACTIVITY_SUMMARY_LIMIT,
    );
    let alert_title = alert.title.clone();
    let alert_body = alert.body.clone();
    let host_id = event.host_id.clone();
    let project_id = event.project_id.clone();
    let thread_id = event.thread_id.clone();
    let notification_thread = format!("{}/{}", event.host_id, event.thread_id);
    let project_title = bounded_activity_text(&event.project_title, ACTIVITY_SUMMARY_LIMIT);
    let thread_title = bounded_activity_text(&event.thread_title, ACTIVITY_SUMMARY_LIMIT);
    let model_title = bounded_activity_text(&event.model_title, ACTIVITY_SUMMARY_LIMIT);
    let phase = serde_json::to_string(&event.phase)
        .map_err(|_| DeliveryError::Provider)?
        .trim_matches('"')
        .to_owned();
    let updated_at = timestamp(event.occurred_at_ms);
    let deep_link = bounded_activity_link(&alert.deep_link);
    let body = serde_json::json!({
        "aps": {
            "alert": {
                "title": alert_title,
                "body": alert_body,
            },
            "sound": "default",
            "thread-id": notification_thread,
        },
        "environmentId": host_id,
        "projectId": project_id,
        "projectTitle": project_title,
        "threadId": thread_id,
        "threadTitle": thread_title,
        "modelTitle": model_title,
        "phase": phase,
        "headline": headline,
        "detail": detail,
        "updatedAt": updated_at,
        "deepLink": deep_link,
    });
    Ok(HttpRequest {
        method: "POST",
        url,
        headers: apns_headers(jwt, bundle_id, "alert", "10"),
        body: bounded_json_body(body)?,
    })
}

fn apns_live_activity_request(
    registration: &RegisterPushDevice,
    state: &PushContentState,
    activity_token: &str,
    event: &str,
    jwt: String,
) -> Result<HttpRequest, DeliveryError> {
    let bundle_id = registration
        .bundle_id
        .as_deref()
        .ok_or(DeliveryError::Unavailable)?;
    let base = match registration.apns_environment {
        Some(ApnsEnvironment::Sandbox) => APNS_SANDBOX,
        Some(ApnsEnvironment::Production) => APNS_PRODUCTION,
        None => return Err(DeliveryError::Unavailable),
    };
    let url = apns_device_url(base, activity_token)?;
    let now = now_seconds();
    let bounded_state = bounded_content_state_value(state)?;
    let alert_title = bounded_state.title.clone();
    let alert_body = bounded_state.subtitle.clone();
    let mut aps = serde_json::json!({
        "timestamp": now,
        "event": event,
        "content-state": bounded_state,
    });
    let aps = aps.as_object_mut().ok_or(DeliveryError::Provider)?;
    match event {
        "start" => {
            aps.insert(
                "attributes-type".into(),
                serde_json::Value::String("AgentActivityAttributes".into()),
            );
            aps.insert("attributes".into(), serde_json::json!({}));
            aps.insert("input-push-token".into(), serde_json::json!(1));
            aps.insert(
                "stale-date".into(),
                serde_json::json!(now.saturating_add(600)),
            );
            aps.insert(
                "alert".into(),
                serde_json::json!({
                    "title": alert_title,
                    "body": alert_body,
                }),
            );
        }
        "update" => {
            aps.insert(
                "stale-date".into(),
                serde_json::json!(now.saturating_add(600)),
            );
        }
        "end" => {
            aps.insert(
                "dismissal-date".into(),
                serde_json::json!(now.saturating_add(300)),
            );
        }
        _ => return Err(DeliveryError::Provider),
    }
    let body = serde_json::json!({ "aps": aps });
    Ok(HttpRequest {
        method: "POST",
        url,
        headers: apns_headers(
            jwt,
            bundle_id,
            "liveactivity",
            if event == "update" { "5" } else { "10" },
        ),
        body: bounded_json_body(body)?,
    })
}

fn apns_headers(
    jwt: String,
    bundle_id: &str,
    push_type: &str,
    priority: &str,
) -> BTreeMap<String, String> {
    let topic = if push_type == "liveactivity" {
        format!("{bundle_id}.push-type.liveactivity")
    } else {
        bundle_id.to_owned()
    };
    BTreeMap::from([
        ("authorization".into(), format!("bearer {jwt}")),
        ("apns-topic".into(), topic),
        ("apns-push-type".into(), push_type.into()),
        ("apns-priority".into(), priority.into()),
        ("content-type".into(), "application/json".into()),
    ])
}

fn apns_device_url(base: &str, token: &str) -> Result<String, DeliveryError> {
    let mut url =
        url::Url::parse(&format!("{base}/3/device/")).map_err(|_| DeliveryError::Provider)?;
    url.path_segments_mut()
        .map_err(|_| DeliveryError::Provider)?
        .push(token);
    Ok(url.to_string())
}

fn apns_jwt(config: &ApnsConfig) -> Result<String, DeliveryError> {
    let key = pem_bytes(&config.private_key).ok_or(DeliveryError::Unavailable)?;
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "alg": "ES256",
            "kid": config.key_id,
            "typ": "JWT",
        })
        .to_string(),
    );
    let now = unix_seconds();
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "iss": config.team_id,
            "iat": now,
        })
        .to_string(),
    );
    let signing_input = format!("{header}.{payload}");
    let rng = SystemRandom::new();
    let key = signature::EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &key,
        &rng,
    )
    .map_err(|_| DeliveryError::Unavailable)?;
    let signature = key
        .sign(&rng, signing_input.as_bytes())
        .map_err(|_| DeliveryError::Unavailable)?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.as_ref())
    ))
}

/// Builds an FCM payload from explicit provider, registration, source event,
/// aggregate, timing, and alert facts. These inputs remain separate because
/// each has a distinct ownership and privacy boundary.
#[allow(clippy::too_many_arguments)]
fn fcm_notification_request(
    project_id: &str,
    registration: &RegisterPushDevice,
    event: Option<&PushActivityEvent>,
    host_id: &str,
    state: &PushContentState,
    activity_expires_at_ms: i64,
    access_token: String,
    delivery_at_ms: i64,
    clear_source_at_ms: Option<i64>,
    alert: Option<&ActivityAlert>,
) -> Result<HttpRequest, DeliveryError> {
    let alert_enabled = alert.is_some();
    let mut data = BTreeMap::from([("environmentId", host_id.to_owned())]);
    if let Some(event) = event {
        let phase = serde_json::to_string(&event.phase)
            .map_err(|_| DeliveryError::Provider)?
            .trim_matches('"')
            .to_owned();
        let detail = bounded_activity_text(
            event.detail.as_deref().unwrap_or(&event.thread_title),
            ACTIVITY_SUMMARY_LIMIT,
        );
        let (alert_title, alert_body) = alert
            .map(|alert| (alert.title.clone(), alert.body.clone()))
            .unwrap_or_else(|| notification_alert(event));
        data.extend([
            ("projectId", event.project_id.clone()),
            (
                "projectTitle",
                bounded_activity_text(&event.project_title, ACTIVITY_SUMMARY_LIMIT),
            ),
            ("threadId", event.thread_id.clone()),
            (
                "threadTitle",
                bounded_activity_text(&event.thread_title, ACTIVITY_SUMMARY_LIMIT),
            ),
            (
                "modelTitle",
                bounded_activity_text(&event.model_title, ACTIVITY_SUMMARY_LIMIT),
            ),
            ("phase", phase),
            (
                "headline",
                bounded_activity_text(&event.headline, ACTIVITY_SUMMARY_LIMIT),
            ),
            ("detail", detail),
            ("alertTitle", alert_title),
            ("alertBody", alert_body),
            (
                "alert",
                if alert_enabled {
                    "1".into()
                } else {
                    "0".into()
                },
            ),
            ("updatedAt", timestamp(event.occurred_at_ms)),
            ("deepLink", bounded_activity_link(&event.deep_link)),
        ]);
    } else {
        data.insert("activity_clear", "1".into());
        data.insert(
            "activity_clear_source_at",
            clear_source_at_ms
                .ok_or(DeliveryError::Provider)?
                .to_string(),
        );
    }
    data.insert("updated_at", delivery_at_ms.to_string());
    if let Some(alert) = alert {
        data.insert("alertId", alert_identity(alert));
        data.insert("alertDeepLink", bounded_activity_link(&alert.deep_link));
    }
    if registration.preferences.live_activities_enabled {
        data.insert("activity", bounded_content_state(state)?);
        data.insert("activity_expires_at", activity_expires_at_ms.to_string());
    }
    let mut android = serde_json::json!({
        "priority": "HIGH",
        "ttl": "300s",
    });
    if !alert_enabled {
        android["collapse_key"] = serde_json::Value::String("agent-activity".into());
    }
    let body = serde_json::json!({
        "message": {
            "token": registration.token,
            "android": android,
            "data": data,
        }
    });
    Ok(HttpRequest {
        method: "POST",
        url: format!("{FCM_ENDPOINT}/{project_id}/messages:send"),
        headers: BTreeMap::from([
            ("authorization".into(), format!("Bearer {access_token}")),
            ("content-type".into(), "application/json".into()),
        ]),
        body: bounded_json_body(body)?,
    })
}

fn alert_identity(alert: &ActivityAlert) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, alert.identity.as_bytes()).as_ref())
}

fn notification_alert(event: &PushActivityEvent) -> (String, String) {
    let title = bounded_activity_text(&event.thread_title, ACTIVITY_SUMMARY_LIMIT);
    let project = bounded_activity_text(&event.project_title, ACTIVITY_SUMMARY_LIMIT);
    let body = bounded_activity_text(
        &format!(
            "{}: {project}",
            agent_domain::activity_status(event.phase.wire_name())
        ),
        ACTIVITY_SUMMARY_LIMIT,
    );
    (title, body)
}

async fn fcm_access_token(
    config: &FcmConfig,
    transport: Arc<dyn PushTransport>,
    stop: CancellationToken,
) -> Result<(String, u64), DeliveryError> {
    let key = pem_bytes(&config.private_key).ok_or(DeliveryError::Unavailable)?;
    let header =
        URL_SAFE_NO_PAD.encode(serde_json::json!({"alg": "RS256", "typ": "JWT"}).to_string());
    let now = unix_seconds();
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "iss": config.client_email,
            "scope": FCM_SCOPE,
            "aud": FCM_AUDIENCE,
            "iat": now,
            "exp": now.saturating_add(3600),
        })
        .to_string(),
    );
    let signing_input = format!("{header}.{payload}");
    let key = signature::RsaKeyPair::from_pkcs8(&key).map_err(|_| DeliveryError::Unavailable)?;
    let mut output = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_input.as_bytes(),
        &mut output,
    )
    .map_err(|_| DeliveryError::Unavailable)?;
    let assertion = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(output));
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")
        .append_pair("assertion", &assertion)
        .finish()
        .into_bytes();
    let response = send_with_retry(
        transport,
        HttpRequest {
            method: "POST",
            url: FCM_TOKEN_ENDPOINT.into(),
            headers: BTreeMap::from([(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )]),
            body,
        },
        stop,
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|_| DeliveryError::Provider)?;
    let token = value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty() && token.len() <= MAX_RESPONSE_BYTES)
        .ok_or(DeliveryError::Provider)?;
    let expires_in = value
        .get("expires_in")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(3_600)
        .clamp(60, 3_600);
    Ok((token.into(), expires_in))
}

async fn send_with_retry(
    transport: Arc<dyn PushTransport>,
    request: HttpRequest,
    stop: CancellationToken,
) -> Result<HttpResponse, DeliveryError> {
    if request.body.len() > MAX_PROVIDER_PAYLOAD_BYTES {
        return Err(DeliveryError::Provider);
    }
    for attempt in 0..MAX_ATTEMPTS {
        let response = tokio::select! {
            _ = stop.cancelled() => return Err(DeliveryError::Cancelled),
            response = transport.send(request.clone()) => {
                match response {
                    Ok(response) => response,
                    Err(_) if attempt + 1 < MAX_ATTEMPTS => {
                        tokio::select! {
                            _ = stop.cancelled() => return Err(DeliveryError::Cancelled),
                            _ = tokio::time::sleep(retry_delay(attempt)) => {}
                        }
                        continue;
                    }
                    Err(_) => return Err(DeliveryError::Transport),
                }
            }
        };
        if (200..300).contains(&response.status) {
            return Ok(response);
        }
        if !retryable_status(response.status) || attempt + 1 == MAX_ATTEMPTS {
            return Err(DeliveryError::Provider);
        }
        tokio::select! {
            _ = stop.cancelled() => return Err(DeliveryError::Cancelled),
            _ = tokio::time::sleep(retry_delay(attempt)) => {}
        }
    }
    Err(DeliveryError::Provider)
}

fn pem_bytes(value: &str) -> Option<Vec<u8>> {
    let value = value.replace("\\n", "\n").replace('\r', "");
    let begin = value.find("-----BEGIN ")?;
    let body_start = value[begin..].find("-----\n")? + begin + 6;
    let end = value[body_start..].find("-----END ")? + body_start;
    let body = value[body_start..end]
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.decode(body).ok()
}

fn bounded_content_state(state: &PushContentState) -> Result<String, DeliveryError> {
    serde_json::to_string(&bounded_content_state_value(state)?).map_err(|_| DeliveryError::Provider)
}

fn bounded_content_state_value(
    state: &PushContentState,
) -> Result<PushContentState, DeliveryError> {
    let full = serde_json::to_string(state).map_err(|_| DeliveryError::Provider)?;
    if full.len() <= MAX_FCM_ACTIVITY_BYTES
        && state.activities.len() <= ACTIVITY_ROWS_LIMIT
        && bounded_activity_text(&state.title, ACTIVITY_SUMMARY_LIMIT) == state.title
        && bounded_activity_text(&state.subtitle, ACTIVITY_SUMMARY_LIMIT) == state.subtitle
        && state.activities.iter().all(activity_fits_bounds)
    {
        return Ok(state.clone());
    }

    let mut compact = state.clone();
    compact.title = bounded_activity_text(&compact.title, ACTIVITY_SUMMARY_LIMIT);
    compact.subtitle = bounded_activity_text(&compact.subtitle, ACTIVITY_SUMMARY_LIMIT);
    // The domain projection has already selected attention rows and ordered
    // them. This layer only fits that canonical state into the provider byte
    // budget; it must not make a second semantic selection.
    let mut activities = compact.activities;
    activities.truncate(ACTIVITY_ROWS_LIMIT);
    compact.activities = Vec::new();
    for activity in activities {
        let mut bounded = activity.clone();
        // Identifiers and deep links are routing data. Truncating them creates
        // a different thread target, so an over-budget row is omitted below.
        if bounded_activity_link(&bounded.deep_link) != bounded.deep_link {
            continue;
        }
        bounded.project_title =
            bounded_activity_text(&bounded.project_title, ACTIVITY_SUMMARY_LIMIT);
        bounded.thread_title = bounded_activity_text(&bounded.thread_title, ACTIVITY_SUMMARY_LIMIT);
        bounded.model_title = bounded_activity_text(&bounded.model_title, ACTIVITY_SUMMARY_LIMIT);
        bounded.status = bounded_activity_text(&bounded.status, ACTIVITY_STATUS_LIMIT);
        let mut candidate = compact.clone();
        candidate.activities.push(bounded.clone());
        if serde_json::to_string(&candidate)
            .map_err(|_| DeliveryError::Provider)?
            .len()
            <= MAX_FCM_ACTIVITY_BYTES
        {
            compact.activities.push(bounded);
        }
    }
    if !compact.activities.is_empty() {
        return Ok(compact);
    }

    let fallback = PushContentState {
        title: bounded_activity_text(&state.title, ACTIVITY_SUMMARY_LIMIT),
        subtitle: bounded_activity_text(&state.subtitle, ACTIVITY_SUMMARY_LIMIT),
        active_count: state.active_count,
        updated_at: state.updated_at.clone(),
        // All rows exceeded the provider bound. Keep the aggregate count and
        // summary while omitting unsafe routing records.
        activities: Vec::new(),
    };
    if serde_json::to_string(&fallback)
        .map_err(|_| DeliveryError::Provider)?
        .len()
        <= MAX_FCM_ACTIVITY_BYTES
    {
        Ok(fallback)
    } else {
        Err(DeliveryError::Provider)
    }
}

fn activity_fits_bounds(activity: &agent_protocol::push::PushActivityItem) -> bool {
    bounded_activity_link(&activity.deep_link) == activity.deep_link
        && bounded_activity_text(&activity.project_title, ACTIVITY_SUMMARY_LIMIT)
            == activity.project_title
        && bounded_activity_text(&activity.thread_title, ACTIVITY_SUMMARY_LIMIT)
            == activity.thread_title
        && bounded_activity_text(&activity.model_title, ACTIVITY_SUMMARY_LIMIT)
            == activity.model_title
        && bounded_activity_text(&activity.status, ACTIVITY_STATUS_LIMIT) == activity.status
}

fn bounded_json_body(body: serde_json::Value) -> Result<Vec<u8>, DeliveryError> {
    let body = serde_json::to_vec(&body).map_err(|_| DeliveryError::Provider)?;
    if body.len() > MAX_PROVIDER_PAYLOAD_BYTES {
        return Err(DeliveryError::Provider);
    }
    Ok(body)
}

fn valid_project_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_seconds() -> u64 {
    unix_seconds()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{
        ACTIVITY_LINK_LIMIT, RUNNING_ACTIVITY_TTL_MS, TERMINAL_ACTIVITY_TTL_MS,
        TERMINAL_NOTIFICATION_FRESHNESS_MS, WAITING_ACTIVITY_TTL_MS,
    };
    use agent_protocol::push::{PushActivityItem, PushPreferences};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeTransport {
        calls: AtomicUsize,
        statuses: Mutex<Vec<u16>>,
    }

    #[async_trait]
    impl PushTransport for FakeTransport {
        async fn send(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let status = self.statuses.lock().await.pop().unwrap_or(200);
            Ok(HttpResponse {
                status,
                body: if status == 400 {
                    br#"{"reason":"BadDeviceToken"}"#.to_vec()
                } else {
                    vec![]
                },
            })
        }
    }

    fn registration() -> RegisterPushDevice {
        RegisterPushDevice {
            device_id: "device".into(),
            platform: PushPlatform::Ios,
            token: "token".into(),
            live_activity_token: None,
            push_to_start_token: None,
            bundle_id: Some("dev.remoteagent.mobile".into()),
            apns_environment: Some(ApnsEnvironment::Sandbox),
            preferences: PushPreferences::default(),
        }
    }

    #[test]
    fn retry_policy_is_bounded_and_only_retries_transient_statuses() {
        assert!(retryable_status(429));
        assert!(retryable_status(503));
        assert!(!retryable_status(400));
        assert_eq!(retry_delay(0), Duration::from_millis(250));
        assert_eq!(retry_delay(MAX_ATTEMPTS), Duration::from_millis(2_000));
    }

    #[test]
    fn invalid_tokens_are_detected_without_printing_body() {
        assert!(invalid_token_response(410, b""));
        assert!(invalid_token_response(
            400,
            br#"{"reason":"BadDeviceToken"}"#
        ));
        assert!(invalid_token_response(
            404,
            br#"{"error":{"status":"UNREGISTERED"}}"#
        ));
        assert!(invalid_token_response(
            404,
            b"registration-token-not-registered"
        ));
        assert!(!invalid_token_response(
            400,
            br#"{"error":{"status":"INVALID_ARGUMENT","message":"The registration token is invalid"}}"#
        ));
        assert!(!invalid_token_response(400, b"invalid topic"));
    }

    #[test]
    fn deep_link_escapes_host_and_thread_segments() {
        assert_eq!(
            thread_deep_link("host/id", "thread id"),
            "remoteagent://threads/host%2Fid/thread%20id"
        );
        assert_eq!(
            bounded_activity_link("remoteagent://threads/host//thread"),
            ""
        );
        assert_eq!(
            bounded_activity_link("remoteagent://threads/host/thread/"),
            ""
        );
    }

    #[test]
    fn activity_content_state_keeps_all_active_rows() {
        let make = |id: &str| PushActivityEvent {
            host_id: "host".into(),
            thread_id: id.into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: id.into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Running,
            headline: "Agent working".into(),
            detail: None,
            deep_link: thread_deep_link("host", id),
            occurred_at_ms: 1,
        };
        let state = content_state(&make("b"), &[make("a")]);
        assert_eq!(state.active_count, 2);
        assert_eq!(state.activities.len(), 2);
        assert_eq!(state.activities[0].thread_id, "a");
    }

    #[test]
    fn activity_expiry_uses_the_visible_canonical_rows() {
        let make =
            |thread_id: &str, phase: PushActivityPhase, occurred_at_ms: i64| PushActivityEvent {
                host_id: "host".into(),
                thread_id: thread_id.into(),
                project_id: "project".into(),
                project_title: "Project".into(),
                thread_title: thread_id.into(),
                model_title: "Model".into(),
                phase,
                headline: "Agent update".into(),
                detail: None,
                deep_link: thread_deep_link("host", thread_id),
                occurred_at_ms,
            };
        let running = make("running", PushActivityPhase::Running, 1_000);
        let waiting = make("waiting", PushActivityPhase::WaitingForInput, 2_000);
        let state = content_state(&waiting, &[running, waiting.clone()]);
        assert_eq!(
            content_state_expiry_at_ms(&state, &waiting),
            2_000 + WAITING_ACTIVITY_TTL_MS
        );
    }

    #[test]
    fn terminal_activity_updates_until_the_aggregate_is_empty() {
        let mut registration = registration();
        registration.live_activity_token = Some("activity".into());
        let mut state = PushContentState {
            title: "Project".into(),
            subtitle: "Agent finished".into(),
            active_count: 1,
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            activities: vec![],
        };
        assert_eq!(
            live_activity_target(&registration, &state, PushActivityPhase::Completed),
            Some(("activity", "update"))
        );
        state.active_count = 0;
        assert_eq!(
            live_activity_target(&registration, &state, PushActivityPhase::Completed),
            Some(("activity", "end"))
        );
    }

    #[test]
    fn push_to_start_marker_prevents_duplicate_starts_until_retry_window() {
        let mut registration = registration();
        registration.push_to_start_token = Some("start".into());
        let state = PushContentState {
            title: "Project".into(),
            subtitle: "Agent is working".into(),
            active_count: 1,
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            activities: vec![],
        };
        let device = StoredDevice {
            principal: "principal".into(),
            registration,
            active: true,
            activity_start_sent: Some("start".into()),
            activity_start_sent_at_ms: Some(current_millis()),
        };
        assert!(
            live_activity_target_for_device(&device, &state, PushActivityPhase::Running).is_none()
        );
        let expired = StoredDevice {
            activity_start_sent_at_ms: Some(current_millis() - PUSH_TO_START_RETRY_AFTER_MS),
            ..device
        };
        assert_eq!(
            live_activity_target_for_device(&expired, &state, PushActivityPhase::Running),
            Some(("start", "start"))
        );
    }

    #[test]
    fn fcm_activity_state_is_bounded_without_truncating_json() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "project".into(),
            thread_title: "thread".into(),
            model_title: "model".into(),
            phase: PushActivityPhase::Running,
            headline: "working".into(),
            detail: None,
            deep_link: "remoteagent://threads/host/thread".into(),
            occurred_at_ms: 1,
        };
        let active = (0..64).map(|_| event.clone()).collect::<Vec<_>>();
        let mut state = content_state(&event, &active);
        state.title = "x".repeat(5_000);
        state.subtitle = "y".repeat(5_000);
        let encoded = bounded_content_state(&state).unwrap();
        assert!(encoded.len() <= MAX_FCM_ACTIVITY_BYTES);
        assert!(serde_json::from_str::<PushContentState>(&encoded).is_ok());
    }

    #[test]
    fn bounded_activity_state_preserves_routing_fields_and_attention_rows() {
        let make = |id: &str, phase: PushActivityPhase| PushActivityItem {
            environment_id: format!("environment-{id}"),
            thread_id: format!("thread-{id}"),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase,
            status: agent_domain::activity_status(phase.wire_name()).into(),
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            deep_link: format!("remoteagent://threads/environment-{id}/thread-{id}"),
        };
        let state = PushContentState {
            title: "Project".into(),
            subtitle: "Agent work".into(),
            active_count: 6,
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            activities: vec![
                make("approval", PushActivityPhase::WaitingForApproval),
                make("running-a", PushActivityPhase::Running),
                make("running-b", PushActivityPhase::Running),
                make("running-c", PushActivityPhase::Running),
                make("running-d", PushActivityPhase::Running),
                make("running-e", PushActivityPhase::Running),
            ],
        };
        let bounded = bounded_content_state_value(&state).unwrap();
        assert_eq!(bounded.active_count, 6);
        assert_eq!(bounded.activities.len(), ACTIVITY_ROWS_LIMIT);
        assert_eq!(
            bounded.activities[0].phase,
            PushActivityPhase::WaitingForApproval
        );
        assert_eq!(bounded.activities[0].thread_id, "thread-approval");
        assert_eq!(
            bounded.activities[0].deep_link,
            "remoteagent://threads/environment-approval/thread-approval"
        );
    }

    #[test]
    fn bounded_activity_state_omits_oversized_links_without_changing_identity() {
        let item = PushActivityItem {
            environment_id: "environment".into(),
            thread_id: "thread".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Running,
            status: "Working".into(),
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            deep_link: format!("remoteagent://threads/{}", "x".repeat(ACTIVITY_LINK_LIMIT)),
        };
        let state = PushContentState {
            title: "Project".into(),
            subtitle: "Agent work".into(),
            active_count: 1,
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            activities: vec![item],
        };
        let bounded = bounded_content_state_value(&state).unwrap();
        assert_eq!(bounded.active_count, 1);
        assert!(bounded.activities.is_empty());
    }

    #[test]
    fn bounded_activity_state_omits_invalid_links_without_changing_identity() {
        let item = PushActivityItem {
            environment_id: "environment".into(),
            thread_id: "thread".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Running,
            status: "Working".into(),
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            deep_link: "https://example.invalid/thread".into(),
        };
        let state = PushContentState {
            title: "Project".into(),
            subtitle: "Agent work".into(),
            active_count: 1,
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            activities: vec![item],
        };
        let bounded = bounded_content_state_value(&state).unwrap();
        assert_eq!(bounded.active_count, 1);
        assert!(bounded.activities.is_empty());
    }

    #[test]
    fn android_notification_only_delivery_omits_the_ongoing_activity_payload() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Completed,
            headline: "Agent finished".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1,
        };
        let state = content_state(&event, &[]);
        let mut registration = registration();
        registration.platform = PushPlatform::Android;
        registration.bundle_id = None;
        registration.apns_environment = None;
        registration.preferences.live_activities_enabled = false;
        let request = fcm_notification_request(
            "project-id",
            &registration,
            Some(&event),
            "host",
            &state,
            900_000,
            "access-token".into(),
            1_800_000_000_000,
            None,
            None,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(body["message"]["data"]["activity"].is_null());
        assert_eq!(body["message"]["data"]["alertTitle"], "Thread");
        assert_eq!(body["message"]["data"]["alert"], "0");
        assert_eq!(body["message"]["data"]["updated_at"], "1800000000000");
        assert_eq!(body["message"]["android"]["priority"], "HIGH");
        assert_eq!(body["message"]["android"]["collapse_key"], "agent-activity");
    }

    #[test]
    fn android_activity_payload_carries_host_absolute_expiry() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Running,
            headline: "Working".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1,
        };
        let state = content_state(&event, &[]);
        let mut registration = registration();
        registration.platform = PushPlatform::Android;
        registration.bundle_id = None;
        registration.apns_environment = None;
        let request = fcm_notification_request(
            "project-id",
            &registration,
            Some(&event),
            "host",
            &state,
            7_200_001,
            "access-token".into(),
            1_800_000_000_000,
            None,
            None,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["message"]["data"]["activity_expires_at"], "7200001");
    }

    #[test]
    fn android_empty_activity_delivery_clears_the_host_without_a_fake_row() {
        let mut registration = registration();
        registration.platform = PushPlatform::Android;
        registration.bundle_id = None;
        registration.apns_environment = None;
        registration.preferences.live_activities_enabled = true;
        let request = fcm_notification_request(
            "project-id",
            &registration,
            None,
            "host",
            &empty_content_state(),
            0,
            "access-token".into(),
            1_800_000_000_000,
            Some(1_799_999_999_000),
            None,
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["message"]["data"]["activity_clear"], "1");
        assert_eq!(
            body["message"]["data"]["activity_clear_source_at"],
            "1799999999000"
        );
        assert_eq!(body["message"]["data"]["activity_expires_at"], "0");
        let activity: serde_json::Value =
            serde_json::from_str(body["message"]["data"]["activity"].as_str().unwrap()).unwrap();
        assert_eq!(activity["activeCount"], 0);
        assert_eq!(activity["updatedAt"], "");
        assert!(activity["activities"].as_array().unwrap().is_empty());
    }

    #[test]
    fn grouped_alert_targets_the_shared_activity_overview() {
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
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1_800_000_000_000,
        };
        let alert = ActivityAlert {
            title: "2 agents need attention".into(),
            body: "One, Two".into(),
            identity: r#"[["host","one","waiting_for_input","2026-01-01T00:00:00.000Z"],["host","two","waiting_for_input","2026-01-01T00:00:01.000Z"]]"#.into(),
            deep_link: agent_domain::ACTIVITY_OVERVIEW_DEEP_LINK.into(),
        };
        let mut registration = registration();
        registration.platform = PushPlatform::Android;
        registration.bundle_id = None;
        registration.apns_environment = None;
        let request = fcm_notification_request(
            "project-id",
            &registration,
            Some(&event),
            "host",
            &content_state(&event, std::slice::from_ref(&event)),
            7_200_001,
            "access-token".into(),
            1_800_000_000_000,
            None,
            Some(&alert),
        )
        .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body["message"]["data"]["alertDeepLink"],
            agent_domain::ACTIVITY_OVERVIEW_DEEP_LINK
        );
        assert!(
            !body["message"]["data"]["alertId"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn empty_delivery_clears_latest_state_without_fabricating_event_data() {
        let root = std::env::temp_dir().join(format!("push-test-{}", uuid::Uuid::new_v4()));
        let fake = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            statuses: Mutex::new(vec![]),
        });
        let service = PushService::with_transport(root.clone(), fake).unwrap();
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Completed,
            headline: "Finished".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1_800_000_000_000,
        };
        let newer = PushActivityEvent {
            thread_id: "newer-thread".into(),
            occurred_at_ms: 1_800_000_001_000,
            ..event.clone()
        };
        assert_eq!(
            clear_source_timestamp(&[event.clone(), newer.clone()]),
            Some(newer.occurred_at_ms)
        );
        *service.latest_state.write().await =
            Some(content_state(&event, std::slice::from_ref(&event)));
        service
            .deliver(1, None, Vec::new(), vec![event, newer], true)
            .await;
        assert!(service.latest_state.read().await.is_none());
        let _ = std::fs::remove_file(root);
    }

    #[test]
    fn activity_rows_expire_by_phase_without_truncating_the_timestamp() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Running,
            headline: "Working".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1_000,
        };
        assert!(!activity_expired(&event, 1_000 + RUNNING_ACTIVITY_TTL_MS));
        assert!(activity_expired(&event, 1_001 + RUNNING_ACTIVITY_TTL_MS));
        let waiting = PushActivityEvent {
            phase: PushActivityPhase::WaitingForInput,
            ..event.clone()
        };
        assert!(!activity_expired(&waiting, 1_000 + WAITING_ACTIVITY_TTL_MS));
        let terminal = PushActivityEvent {
            phase: PushActivityPhase::Completed,
            ..event
        };
        assert!(activity_expired(
            &terminal,
            1_001 + TERMINAL_ACTIVITY_TTL_MS
        ));
    }

    #[test]
    fn event_changed_refreshes_expiry_when_only_source_timestamp_moves() {
        let previous = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Running,
            headline: "Working".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 100,
        };
        let unchanged = previous.clone();
        let refreshed = PushActivityEvent {
            occurred_at_ms: 101,
            ..previous.clone()
        };
        assert!(!event_changed(&previous, &unchanged));
        assert!(event_changed(&previous, &refreshed));
    }

    #[test]
    fn old_terminal_rows_update_activity_without_alerting() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Completed,
            headline: "Agent finished".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1_000,
        };
        assert!(!agent_domain::activity_notification_is_fresh(
            event.phase.wire_name(),
            event.occurred_at_ms,
            1_000 + TERMINAL_NOTIFICATION_FRESHNESS_MS + 1,
        ));
        assert!(agent_domain::activity_notification_is_fresh(
            event.phase.wire_name(),
            event.occurred_at_ms,
            1_000 + TERMINAL_NOTIFICATION_FRESHNESS_MS,
        ));
        assert!(agent_domain::activity_notification_is_fresh(
            PushActivityPhase::Running.wire_name(),
            event.occurred_at_ms,
            i64::MAX,
        ));
    }

    #[test]
    fn activity_subtitle_counts_live_rows_only() {
        let event = PushActivityEvent {
            host_id: "host".into(),
            thread_id: "thread".into(),
            project_id: "project".into(),
            project_title: "Project".into(),
            thread_title: "Thread".into(),
            model_title: "Model".into(),
            phase: PushActivityPhase::Completed,
            headline: "Agent finished".into(),
            detail: None,
            deep_link: thread_deep_link("host", "thread"),
            occurred_at_ms: 1,
        };
        let active = PushActivityEvent {
            thread_id: "active".into(),
            phase: PushActivityPhase::Running,
            headline: "Agent is working".into(),
            deep_link: thread_deep_link("host", "active"),
            ..event.clone()
        };
        let state = content_state(&event, &[event.clone(), active]);
        assert_eq!(state.active_count, 1);
        assert_eq!(state.subtitle, "1 active agent activities");
    }

    #[tokio::test]
    async fn registration_starts_active_only_when_delivery_is_requested() {
        let root = std::env::temp_dir().join(format!("push-test-{}", uuid::Uuid::new_v4()));
        let fake = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            statuses: Mutex::new(vec![]),
        });
        let service = PushService::with_transport(root.clone(), fake).unwrap();
        service.register("principal", registration()).await.unwrap();
        assert!(
            service
                .devices
                .lock()
                .await
                .devices
                .get("device")
                .is_some_and(|device| device.active)
        );
        let mut disabled = registration();
        disabled.preferences.notifications_enabled = false;
        disabled.preferences.live_activities_enabled = false;
        service.register("principal", disabled).await.unwrap();
        assert!(
            !service
                .devices
                .lock()
                .await
                .devices
                .get("device")
                .is_some_and(|device| device.active)
        );
        let _ = std::fs::remove_file(root);
    }

    #[tokio::test]
    async fn shared_activity_start_token_keeps_host_registrations_independent() {
        let root = std::env::temp_dir().join(format!("push-test-{}", uuid::Uuid::new_v4()));
        let fake = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            statuses: Mutex::new(vec![]),
        });
        let service = PushService::with_transport(root.clone(), fake).unwrap();

        let mut first = registration();
        first.device_id = "device-a".into();
        first.push_to_start_token = Some("shared-start-token".into());
        let mut second = registration();
        second.device_id = "device-b".into();
        second.push_to_start_token = Some("shared-start-token".into());
        service.register("principal-a", first).await.unwrap();
        service.register("principal-b", second).await.unwrap();

        let state = PushContentState {
            title: "Project".into(),
            subtitle: "Agent is working".into(),
            active_count: 1,
            updated_at: "2026-10-08T00:00:00.000Z".into(),
            activities: vec![],
        };
        let devices = service.devices.lock().await.devices.clone();
        assert_eq!(devices.len(), 2);
        for device_id in ["device-a", "device-b"] {
            assert_eq!(
                live_activity_target_for_device(
                    &devices[device_id],
                    &state,
                    PushActivityPhase::Running
                ),
                Some(("shared-start-token", "start")),
            );
        }

        service.remove_principal("principal-a").await.unwrap();
        let devices = service.devices.lock().await.devices.clone();
        assert!(devices.contains_key("device-b"));
        assert!(!devices.contains_key("device-a"));
        let _ = std::fs::remove_file(root);
    }

    #[tokio::test]
    async fn invalid_apns_response_removes_normal_registration() {
        let root = std::env::temp_dir().join(format!("push-test-{}", uuid::Uuid::new_v4()));
        let fake = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            statuses: Mutex::new(vec![400]),
        });
        let service = PushService::with_transport(root.clone(), fake).unwrap();
        service.register("principal", registration()).await.unwrap();
        let result = service
            .send_request(HttpRequest {
                method: "POST",
                url: "https://example.invalid".into(),
                headers: BTreeMap::new(),
                body: Vec::new(),
            })
            .await;
        assert_eq!(result, Err(DeliveryError::InvalidToken));
        service
            .invalidate(
                vec![TokenMatch {
                    device_id: "device".into(),
                    token: "token".into(),
                }],
                vec![],
                vec![],
                vec![],
            )
            .await;
        assert!(service.devices.lock().await.devices.is_empty());
        let _ = std::fs::remove_file(root);
    }

    #[tokio::test]
    async fn stale_token_invalidation_does_not_remove_a_new_registration() {
        let root = std::env::temp_dir().join(format!("push-test-{}", uuid::Uuid::new_v4()));
        let fake = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            statuses: Mutex::new(vec![]),
        });
        let service = PushService::with_transport(root.clone(), fake).unwrap();
        service.register("principal", registration()).await.unwrap();
        let mut replacement = registration();
        replacement.token = "new-token".into();
        service.register("principal", replacement).await.unwrap();
        service
            .invalidate(
                vec![TokenMatch {
                    device_id: "device".into(),
                    token: "token".into(),
                }],
                vec![],
                vec![],
                vec![],
            )
            .await;
        assert_eq!(
            service
                .devices
                .lock()
                .await
                .devices
                .get("device")
                .map(|device| device.registration.token.as_str()),
            Some("new-token")
        );
        let _ = std::fs::remove_file(root);
    }
}
