//! Device state the views read beside the Host's: preferences, holds over the
//! list, answer drafts, panel selections and flows in progress.
use crate::view::projects::import::{ImportToast, SessionImportProgress};
use crate::view::{
    models::ordering::FavoriteModel, requests::QuestionDraft, thread_order::PendingThreadOrder,
    time::TimestampFormat,
};
use agent_domain::{CommandId, ThreadId};
use agent_protocol::conversation::SessionScan;
use agent_protocol::device::{
    DeviceAccessibilityTree, DeviceDetail, DeviceEvent, DeviceEventLogEntry,
    DeviceForegroundUpdate, DeviceFrame, DeviceRecording, DeviceScreenConfig, DeviceScreenshot,
    DeviceServiceState, DeviceSession, DeviceVideoFrame,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_VIDEO_EVENTS_PER_STREAM: usize = 64;

/// The source facts needed to send one keyboard event to a device.  Native
/// surfaces should provide the physical code and the platform's semantic key
/// value; this pure normalization only fills in aliases and derives the
/// conventional physical code when a surface (such as GPUI) supplies a key
/// name without one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DeviceKeyFacts {
    pub code: String,
    pub key: String,
}

/// Modifier facts reported by a native keyboard surface for one event.
///
/// The Host wire protocol already represents modifier keys as ordinary
/// physical key codes.  These facts stay in the core intent so each native
/// surface can reconcile its current modifier set with the previous one
/// without reimplementing platform-specific ordering or cleanup rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DeviceModifierFacts {
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub ctrl: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceModifierTransition {
    pub code: String,
    pub down: bool,
}

/// Immutable ownership for a native input stream.  A reconnect gets a new
/// session epoch, so a delayed key reply can never clear or advance the new
/// session's pressed-key state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceInputTarget {
    pub thread_id: ThreadId,
    pub host_id: String,
    pub device_id: String,
    pub session_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceInputPlan {
    pub inputs: Vec<agent_protocol::device::DeviceInput>,
    pub target: DeviceInputTarget,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DeviceInputState {
    modifiers: DeviceModifierFacts,
    pressed: BTreeSet<String>,
}

/// Returns the physical modifier key transitions needed to move between two
/// native surface snapshots.  Presses use a stable left-side code and release
/// in reverse order, which also makes a blur/disconnect reset deterministic.
fn device_modifier_transitions(
    previous: DeviceModifierFacts,
    current: DeviceModifierFacts,
) -> Vec<DeviceModifierTransition> {
    let modifiers = [
        ("ControlLeft", previous.ctrl, current.ctrl),
        ("ShiftLeft", previous.shift, current.shift),
        ("AltLeft", previous.alt, current.alt),
        ("MetaLeft", previous.meta, current.meta),
    ];
    let mut transitions = modifiers
        .iter()
        .filter(|(_, was_down, is_down)| !*was_down && *is_down)
        .map(|(code, _, _)| DeviceModifierTransition {
            code: (*code).to_owned(),
            down: true,
        })
        .collect::<Vec<_>>();
    transitions.extend(
        modifiers
            .iter()
            .rev()
            .filter(|(_, was_down, is_down)| *was_down && !*is_down)
            .map(|(code, _, _)| DeviceModifierTransition {
                code: (*code).to_owned(),
                down: false,
            }),
    );
    transitions
}

fn is_modifier_code(code: &str) -> bool {
    matches!(
        code,
        "ShiftLeft"
            | "ShiftRight"
            | "AltLeft"
            | "AltRight"
            | "ControlLeft"
            | "ControlRight"
            | "MetaLeft"
            | "MetaRight"
    )
}

fn set_modifier_code(facts: &mut DeviceModifierFacts, code: &str, down: bool) {
    match code {
        "ShiftLeft" | "ShiftRight" => facts.shift = down,
        "AltLeft" | "AltRight" => facts.alt = down,
        "ControlLeft" | "ControlRight" => facts.ctrl = down,
        "MetaLeft" | "MetaRight" => facts.meta = down,
        _ => {}
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn canonical_device_key(code: &str, key: &str) -> DeviceKeyFacts {
    let key = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Enter".into(),
        "tab" => "Tab".into(),
        "backspace" => "Backspace".into(),
        "delete" | "forwarddelete" => "Delete".into(),
        "escape" | "esc" => "Escape".into(),
        "shift" | "shiftleft" => "ShiftLeft".into(),
        "control" | "ctrl" | "controlleft" => "ControlLeft".into(),
        "alt" | "option" | "altleft" => "AltLeft".into(),
        "meta" | "command" | "cmd" | "metaleft" => "MetaLeft".into(),
        "up" | "arrowup" => "ArrowUp".into(),
        "down" | "arrowdown" => "ArrowDown".into(),
        "left" | "arrowleft" => "ArrowLeft".into(),
        "right" | "arrowright" => "ArrowRight".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "space" => " ".into(),
        _ => key.to_owned(),
    };
    let code = canonical_device_code(code, &key);
    DeviceKeyFacts { code, key }
}

fn canonical_device_code(code: &str, key: &str) -> String {
    let code = code.trim();
    if !code.is_empty() {
        let lower_code = code.to_ascii_lowercase();
        if let Some(canonical) = match lower_code.as_str() {
            "enter" | "return" => Some("Enter"),
            "tab" => Some("Tab"),
            "backspace" => Some("Backspace"),
            "delete" | "forwarddelete" => Some("Delete"),
            "escape" | "esc" => Some("Escape"),
            "shift" | "shiftleft" => Some("ShiftLeft"),
            "control" | "ctrl" | "controlleft" => Some("ControlLeft"),
            "alt" | "option" | "altleft" => Some("AltLeft"),
            "meta" | "command" | "cmd" | "metaleft" => Some("MetaLeft"),
            "up" | "arrowup" => Some("ArrowUp"),
            "down" | "arrowdown" => Some("ArrowDown"),
            "left" | "arrowleft" => Some("ArrowLeft"),
            "right" | "arrowright" => Some("ArrowRight"),
            "home" => Some("Home"),
            "end" => Some("End"),
            "pageup" => Some("PageUp"),
            "pagedown" => Some("PageDown"),
            "space" => Some("Space"),
            _ => None,
        } {
            return canonical.to_owned();
        }
        let looks_like_key_alias = lower_code.len() == 1
            || matches!(
                lower_code.as_str(),
                "enter"
                    | "return"
                    | "tab"
                    | "backspace"
                    | "delete"
                    | "forwarddelete"
                    | "escape"
                    | "esc"
                    | "up"
                    | "down"
                    | "left"
                    | "right"
                    | "arrowup"
                    | "arrowdown"
                    | "arrowleft"
                    | "arrowright"
                    | "home"
                    | "end"
                    | "pageup"
                    | "pagedown"
                    | "space"
            );
        if !looks_like_key_alias {
            return code.to_owned();
        }
    }
    let lower = key.to_ascii_lowercase();
    if lower.chars().count() == 1 {
        let character = lower.as_bytes()[0];
        if character.is_ascii_lowercase() {
            return format!("Key{}", (character as char).to_ascii_uppercase());
        }
        if character.is_ascii_digit() {
            return format!("Digit{}", character as char);
        }
    }
    match lower.as_str() {
        "!" => "Digit1",
        "@" => "Digit2",
        "#" => "Digit3",
        "$" => "Digit4",
        "%" => "Digit5",
        "^" => "Digit6",
        "&" => "Digit7",
        "*" => "Digit8",
        "(" => "Digit9",
        ")" => "Digit0",
        "-" | "_" => "Minus",
        "=" | "+" => "Equal",
        "[" | "{" => "BracketLeft",
        "]" | "}" => "BracketRight",
        "\\" | "|" => "Backslash",
        ";" | ":" => "Semicolon",
        "'" | "\"" => "Quote",
        "`" | "~" => "Backquote",
        "," | "<" => "Comma",
        "." | ">" => "Period",
        "/" | "?" => "Slash",
        "enter" => "Enter",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "escape" => "Escape",
        "shift" | "shiftleft" => "ShiftLeft",
        "control" | "ctrl" | "controlleft" => "ControlLeft",
        "alt" | "option" | "altleft" => "AltLeft",
        "meta" | "command" | "cmd" | "metaleft" => "MetaLeft",
        "arrowup" => "ArrowUp",
        "arrowdown" => "ArrowDown",
        "arrowleft" => "ArrowLeft",
        "arrowright" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        " " => "Space",
        _ => {
            if code.is_empty() {
                "Unidentified"
            } else {
                code
            }
        }
    }
    .into()
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDuoControlState {
    pub pending: bool,
    pub requested: Option<crate::state::DeviceDuoCommandIntent>,
    pub error: Option<String>,
    queued: Option<crate::state::DeviceDuoCommandIntent>,
    active_request_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDuoRequest {
    pub thread_id: ThreadId,
    pub host_id: Option<String>,
    pub device_id: String,
    pub session_epoch: String,
    pub request_id: u64,
    pub command: crate::state::DeviceDuoCommandIntent,
}

/// Settings this device keeps across launches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    /// Device-local Preview browser defaults, recording options, and profiles.
    pub browser: crate::view::browser::BrowserSettings,
    pub timestamp_format: TimestampFormat,
    pub favorite_models: Vec<FavoriteModel>,
    /// The user's model order per provider instance, by slug.
    pub model_order: BTreeMap<String, Vec<String>>,
    /// The Working section (beta).
    pub working_section: bool,
    pub diff_ignore_whitespace: bool,
    /// The script each project last ran, by project id.
    pub last_run_scripts: BTreeMap<String, String>,
    /// Provider instances whose resume dialog was told never to ask again.
    pub resume_compaction_dismissed: BTreeSet<String>,
    /// The terminal's text size in points; `None` keeps the default.
    pub terminal_font_size: Option<f64>,
    /// Each model's last chosen options, which a newly picked model takes.
    pub model_options: crate::view::models::staging::ModelOptionMemory,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            browser: crate::view::browser::BrowserSettings::default(),
            timestamp_format: TimestampFormat::default(),
            favorite_models: vec![],
            model_order: BTreeMap::new(),
            working_section: false,
            diff_ignore_whitespace: true,
            last_run_scripts: BTreeMap::new(),
            resume_compaction_dismissed: BTreeSet::new(),
            terminal_font_size: None,
            model_options: BTreeMap::new(),
        }
    }
}

/// A list reorder held on screen until its key writes land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadOrderHold {
    pub order: PendingThreadOrder,
    pub commands: Vec<CommandId>,
}

/// One question request's answers being written, and the question shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionDrafts {
    pub drafts: Vec<QuestionDraft>,
    pub question_index: u32,
}
impl QuestionDrafts {
    pub fn get(&self, question_id: &str) -> Option<&QuestionDraft> {
        self.drafts
            .iter()
            .find(|draft| draft.question_id == question_id)
    }
    pub fn set(&mut self, draft: QuestionDraft) {
        match self
            .drafts
            .iter_mut()
            .find(|existing| existing.question_id == draft.question_id)
        {
            Some(existing) => *existing = draft,
            None => self.drafts.push(draft),
        }
    }
}

/// The draft key holding the files attached to one question's answer.
pub fn answer_draft_key(request_id: &str, question_id: &str) -> String {
    format!("answer:{request_id}:{question_id}")
}

/// The agent-session import step of adding projects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionImport {
    pub scan: Option<SessionScan>,
    pub scan_pending: bool,
    pub scan_error: Option<String>,
    /// `None` until the user changes the default selection.
    pub selection: Option<BTreeSet<String>>,
    pub importing: bool,
    pub progress: SessionImportProgress,
    /// Shown once the landing project opened.
    pub toast: Option<ImportToast>,
}

/// Host-owned device state folded into the client snapshot. Device commands
/// remain typed protocol calls; this record only retains the latest state and
/// frames for native views.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceState {
    pub service: Option<DeviceServiceState>,
    /// Service revisions describe discovery/configuration. This revision is
    /// advanced by accepted frame events and stream pruning for native redraws.
    pub frame_revision: u64,
    pub sessions: Vec<DeviceSession>,
    pub details: BTreeMap<(String, String), DeviceDetail>,
    pub frames: BTreeMap<(String, String, String), DeviceFrame>,
    pub video_frames: BTreeMap<(String, String, String, u8), DeviceVideoFrame>,
    /// Ordered access units for stateful native decoders. `video_frames` is
    /// the latest-frame projection used by lightweight still-image consumers.
    pub video_events: BTreeMap<(String, String, String, u8), VecDeque<DeviceVideoFrame>>,
    pub accessibility: BTreeMap<(String, String), DeviceAccessibilityTree>,
    pub event_log: BTreeMap<(String, String), Vec<DeviceEventLogEntry>>,
    pub foreground: BTreeMap<(String, String), DeviceForegroundUpdate>,
    pub screens: BTreeMap<(String, String, String, u8), DeviceScreenConfig>,
    pub recordings:
        BTreeMap<(String, String, String), agent_protocol::device::DeviceRecordingStatus>,
    pub duo_controls: BTreeMap<(String, String, String, String), DeviceDuoControlState>,
    input_state: BTreeMap<(String, String, String, String), DeviceInputState>,
    duo_request_sequence: u64,
    /// The last active recording for a closed session, retained so a delayed
    /// completion from that exact lifetime can still be surfaced while a
    /// completion from a later reopen is rejected.
    closed_recordings: BTreeMap<(String, String, String), (u64, String)>,
    pub last_recording: Option<DeviceRecording>,
    pub last_screenshot: Option<DeviceScreenshot>,
    pub error: Option<String>,
}

impl DeviceState {
    fn input_target(
        &self,
        thread_id: &ThreadId,
        host_id: Option<&str>,
        device_id: &str,
        session_epoch: Option<&str>,
    ) -> Result<(DeviceInputTarget, agent_protocol::device::DevicePlatform), String> {
        let effective_host = host_id.unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let session = self
            .sessions
            .iter()
            .find(|session| {
                &session.thread_id == thread_id
                    && session.host_id == effective_host
                    && session.device_id == device_id
                    && session_epoch.is_none_or(|epoch| session.session_epoch == epoch)
            })
            .ok_or_else(|| "device session is not open".to_owned())?;
        Ok((
            DeviceInputTarget {
                thread_id: thread_id.clone(),
                host_id: session.host_id.clone(),
                device_id: session.device_id.clone(),
                session_epoch: session.session_epoch.clone(),
            },
            session.platform,
        ))
    }

    /// Resolves the current session once for a generic device command.  The
    /// returned target is copied into the wire request so a queued command
    /// cannot be redirected to a replacement session with the same device id.
    pub(crate) fn session_target(
        &self,
        thread_id: &ThreadId,
        host_id: Option<&str>,
        device_id: &str,
    ) -> Result<DeviceInputTarget, String> {
        self.input_target(thread_id, host_id, device_id, None)
            .map(|(target, _)| target)
    }

    fn input_key(target: &DeviceInputTarget) -> (String, String, String, String) {
        (
            target.thread_id.to_string(),
            target.host_id.clone(),
            target.device_id.clone(),
            target.session_epoch.clone(),
        )
    }

    fn key_input(
        target: &DeviceInputTarget,
        code: String,
        key: String,
        down: bool,
        meta: bool,
        ctrl: bool,
    ) -> agent_protocol::device::DeviceInput {
        agent_protocol::device::DeviceInput {
            thread_id: target.thread_id.clone(),
            host_id: Some(target.host_id.clone()),
            device_id: target.device_id.clone(),
            session_epoch: target.session_epoch.clone(),
            input: agent_protocol::device::DeviceInputKind::Key {
                code,
                key,
                down,
                meta,
                ctrl,
            },
        }
    }

    /// Builds the ordered wire events for one native key observation and
    /// records the resulting pressed-key state under the current session
    /// epoch. iOS receives physical modifier transitions; Android keeps its
    /// source semantic key path and does not receive unsupported modifier
    /// pseudo-characters.
    pub fn key_input_plan(
        &mut self,
        thread_id: ThreadId,
        host_id: Option<String>,
        device_id: String,
        code: String,
        key: String,
        session_epoch: String,
        down: bool,
        modifiers: DeviceModifierFacts,
    ) -> Result<DeviceInputPlan, String> {
        let (target, platform) =
            self.input_target(
                &thread_id,
                host_id.as_deref(),
                &device_id,
                Some(session_epoch.as_str()),
            )?;
        let facts = canonical_device_key(&code, &key);
        let mut current_modifiers = modifiers;
        if is_modifier_code(&facts.code) {
            set_modifier_code(&mut current_modifiers, &facts.code, down);
        }
        let state_key = Self::input_key(&target);
        let previous = self.input_state.get(&state_key).cloned().unwrap_or_default();
        let mut inputs = Vec::new();
        if platform == agent_protocol::device::DevicePlatform::Ios {
            for transition in device_modifier_transitions(previous.modifiers, current_modifiers) {
                inputs.push(Self::key_input(
                    &target,
                    transition.code.clone(),
                    transition.code,
                    transition.down,
                    false,
                    false,
                ));
            }
        }
        if !is_modifier_code(&facts.code) {
            inputs.push(Self::key_input(
                &target,
                facts.code.clone(),
                facts.key,
                down,
                current_modifiers.meta,
                current_modifiers.ctrl,
            ));
        }
        let mut next = previous;
        next.modifiers = current_modifiers;
        if !is_modifier_code(&facts.code) {
            if down {
                next.pressed.insert(facts.code);
            } else {
                next.pressed.remove(&facts.code);
            }
        }
        self.input_state.insert(state_key, next);
        Ok(DeviceInputPlan { inputs, target })
    }

    /// Releases every key owned by a native surface, including ordinary keys
    /// that were held when focus moved. The target is resolved before the
    /// state is removed so a close/reconnect cannot redirect releases.
    pub fn release_input_plan(
        &mut self,
        thread_id: ThreadId,
        host_id: Option<String>,
        device_id: String,
        session_epoch: Option<String>,
    ) -> Result<Option<DeviceInputPlan>, String> {
        let (target, platform) = self.input_target(
            &thread_id,
            host_id.as_deref(),
            &device_id,
            session_epoch.as_deref(),
        )?;
        let Some(previous) = self.input_state.remove(&Self::input_key(&target)) else {
            return Ok(None);
        };
        let mut inputs = if platform == agent_protocol::device::DevicePlatform::Ios {
            device_modifier_transitions(previous.modifiers, DeviceModifierFacts::default())
                .into_iter()
                .map(|transition| {
                    Self::key_input(
                        &target,
                        transition.code.clone(),
                        transition.code,
                        false,
                        false,
                        false,
                    )
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for code in previous.pressed {
            inputs.push(Self::key_input(
                &target,
                code.clone(),
                code,
                false,
                false,
                false,
            ));
        }
        Ok(Some(DeviceInputPlan { inputs, target }))
    }

    pub fn clear_input_state(&mut self, target: &DeviceInputTarget) {
        self.input_state.remove(&Self::input_key(target));
    }

    /// Builds releases for every native input owner in one thread before its
    /// device subscription or thread view is torn down.  Session identity is
    /// kept from the owned key, so a reconnect cannot redirect the cleanup to
    /// a replacement session.
    pub fn release_input_plans_for_thread(
        &mut self,
        thread_id: &ThreadId,
    ) -> Vec<DeviceInputPlan> {
        let thread_key = thread_id.to_string();
        let targets = self
            .input_state
            .keys()
            .filter(|(thread, _, _, _)| thread == &thread_key)
            .map(|(_, host_id, device_id, session_epoch)| {
                (host_id.clone(), device_id.clone(), session_epoch.clone())
            })
            .collect::<Vec<_>>();
        targets
            .into_iter()
            .filter_map(|(host_id, device_id, session_epoch)| {
                let state_key = (
                    thread_key.clone(),
                    host_id.clone(),
                    device_id.clone(),
                    session_epoch.clone(),
                );
                let plan = self.release_input_plan(
                    thread_id.clone(),
                    Some(host_id),
                    device_id,
                    Some(session_epoch),
                );
                match plan {
                    Ok(plan) => plan,
                    Err(_) => {
                        // The Host may have closed the session before the
                        // stream teardown reached the owner.  There is no
                        // valid target left to release on the wire, but the
                        // local ownership record must still be discarded.
                        self.input_state.remove(&state_key);
                        None
                    }
                }
            })
            .collect()
    }

    pub fn clear_input_state_on_disconnect(&mut self) {
        self.input_state.clear();
    }

    /// Returns whether a planned native input still targets the live session
    /// that owned it.  The connection owner checks this immediately before
    /// admitting the wire job, so a thread switch or reconnect cannot send a
    /// queued release or key to a replacement session.
    pub fn accepts_input_target(&self, target: &DeviceInputTarget) -> bool {
        self.sessions.iter().any(|session| {
            session.thread_id == target.thread_id
                && session.host_id == target.host_id
                && session.device_id == target.device_id
                && session.session_epoch == target.session_epoch
        })
    }

    pub fn enqueue_duo(
        &mut self,
        thread_id: ThreadId,
        host_id: Option<String>,
        device_id: String,
        command: crate::state::DeviceDuoCommandIntent,
    ) -> Result<Option<DeviceDuoRequest>, String> {
        let effective_host = host_id
            .as_deref()
            .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID);
        let session = self
            .sessions
            .iter()
            .find(|session| {
                session.thread_id == thread_id
                    && session.host_id == effective_host
                    && session.device_id == device_id
            })
            .cloned()
            .ok_or_else(|| "device session is not open".to_owned())?;
        let key = (
            thread_id.to_string(),
            effective_host.to_owned(),
            device_id.clone(),
            session.session_epoch.clone(),
        );
        if self
            .duo_controls
            .get(&key)
            .is_some_and(|control| control.pending)
        {
            self.duo_controls
                .get_mut(&key)
                .expect("Duo control state exists")
                .queued = Some(command);
            return Ok(None);
        }
        self.duo_request_sequence = self.duo_request_sequence.saturating_add(1).max(1);
        let request_id = self.duo_request_sequence;
        let control = self
            .duo_controls
            .entry(key)
            .or_insert_with(|| DeviceDuoControlState {
                pending: false,
                requested: None,
                error: None,
                queued: None,
                active_request_id: None,
            });
        control.error = None;
        control.pending = true;
        control.requested = Some(command.clone());
        control.active_request_id = Some(request_id);
        Ok(Some(DeviceDuoRequest {
            thread_id,
            host_id,
            device_id,
            session_epoch: session.session_epoch,
            request_id,
            command,
        }))
    }

    pub fn complete_duo(
        &mut self,
        request: &DeviceDuoRequest,
        accepted: bool,
        error: Option<String>,
    ) -> Option<DeviceDuoRequest> {
        let key = (
            request.thread_id.to_string(),
            request
                .host_id
                .as_deref()
                .unwrap_or(agent_protocol::device::LOCAL_DEVICE_HOST_ID)
                .to_owned(),
            request.device_id.clone(),
            request.session_epoch.clone(),
        );
        if !self.accepts_duo_request(request) {
            self.duo_controls.remove(&key);
            return None;
        }
        let queued = {
            let control = self.duo_controls.get_mut(&key)?;
            if control.active_request_id != Some(request.request_id) {
                return None;
            }
            if !accepted {
                control.pending = false;
                control.requested = None;
                control.queued = None;
                control.active_request_id = None;
                control.error = Some(error.unwrap_or_else(|| "device Duo control failed".into()));
                return None;
            }
            control.queued.take()
        };
        if let Some(command) = queued {
            self.duo_request_sequence = self.duo_request_sequence.saturating_add(1).max(1);
            let request_id = self.duo_request_sequence;
            let control = self
                .duo_controls
                .get_mut(&key)
                .expect("Duo control state exists");
            control.requested = Some(command.clone());
            control.active_request_id = Some(request_id);
            return Some(DeviceDuoRequest {
                thread_id: request.thread_id.clone(),
                host_id: request.host_id.clone(),
                device_id: request.device_id.clone(),
                session_epoch: request.session_epoch.clone(),
                request_id,
                command,
            });
        }
        self.duo_controls.remove(&key);
        None
    }

    pub fn fail_duo(&mut self, request: &DeviceDuoRequest, error: impl Into<String>) {
        let _ = self.complete_duo(request, false, Some(error.into()));
    }

    pub fn clear_duo_for_closed_sessions(&mut self) {
        let sessions = self.sessions.clone();
        self.duo_controls
            .retain(|(thread, host, device, epoch), _| {
                sessions.iter().any(|session| {
                    session.thread_id.to_string() == *thread
                        && session.host_id == *host
                        && session.device_id == *device
                        && session.session_epoch == *epoch
                })
            });
    }

    /// A Host connection epoch owns every in-flight Duo request. Once that
    /// connection is gone, the request receipts cannot be completed by a
    /// later connection and must not leave a pending control or promote a
    /// stale queued command after reconnect.
    pub fn clear_duo_on_disconnect(&mut self) {
        self.duo_controls.clear();
    }

    fn accepts_thread_event(
        &self,
        thread_id: &agent_domain::ThreadId,
        host_id: &str,
        device_id: &str,
        epoch: &str,
    ) -> bool {
        self.sessions.iter().any(|session| {
            &session.thread_id == thread_id
                && session.host_id == host_id
                && session.device_id == device_id
                && session.session_epoch == epoch
        })
    }

    fn accepts_device_event(&self, host_id: &str, device_id: &str, epoch: &str) -> bool {
        self.sessions.iter().any(|session| {
            session.host_id == host_id
                && session.device_id == device_id
                && session.session_epoch == epoch
        })
    }

    pub fn apply_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::State(service) => {
                self.sessions = service.sessions.clone();
                self.service = Some(service);
                let mut frame_stream_changed = false;
                let active = self
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            session.thread_id.to_string(),
                            session.host_id.clone(),
                            session.device_id.clone(),
                            session.session_epoch.clone(),
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                self.frames.retain(|key, frame| {
                    let keep = active.contains(&(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        frame.session_epoch.clone(),
                    ));
                    frame_stream_changed |= !keep;
                    keep
                });
                self.video_frames.retain(|key, frame| {
                    let keep = active.contains(&(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        frame.session_epoch.clone(),
                    ));
                    frame_stream_changed |= !keep;
                    keep
                });
                self.video_events.retain(|key, events| {
                    events.retain(|frame| {
                        let keep = active.contains(&(
                            key.0.clone(),
                            key.1.clone(),
                            key.2.clone(),
                            frame.session_epoch.clone(),
                        ));
                        frame_stream_changed |= !keep;
                        keep
                    });
                    let keep = !events.is_empty();
                    frame_stream_changed |= !keep;
                    keep
                });
                self.input_state.retain(|key, _| active.contains(key));
                let active_devices = self
                    .sessions
                    .iter()
                    .map(|session| (session.host_id.clone(), session.device_id.clone()))
                    .collect::<std::collections::BTreeSet<_>>();
                let active_epochs = self.sessions.iter().fold(
                    BTreeMap::<(String, String), BTreeSet<String>>::new(),
                    |mut epochs, session| {
                        epochs
                            .entry((session.host_id.clone(), session.device_id.clone()))
                            .or_default()
                            .insert(session.session_epoch.clone());
                        epochs
                    },
                );
                self.details.retain(|key, _| active_devices.contains(key));
                self.accessibility.retain(|key, tree| {
                    active_devices.contains(key)
                        && active_epochs
                            .get(key)
                            .is_some_and(|epochs| epochs.contains(&tree.session_epoch))
                });
                self.event_log.retain(|key, entries| {
                    if !active_devices.contains(key) {
                        return false;
                    }
                    entries.retain(|entry| {
                        active_epochs
                            .get(key)
                            .is_some_and(|epochs| epochs.contains(&entry.session_epoch))
                    });
                    !entries.is_empty()
                });
                self.foreground.retain(|key, update| {
                    active_devices.contains(key)
                        && active_epochs
                            .get(key)
                            .is_some_and(|epochs| epochs.contains(&update.session_epoch))
                });
                self.screens.retain(|key, screen| {
                    active.contains(&(
                        key.0.clone(),
                        key.1.clone(),
                        key.2.clone(),
                        screen.session_epoch.clone(),
                    ))
                });
                let removed_recordings = self
                    .recordings
                    .iter()
                    .filter_map(|(key, status)| {
                        let keep = active.iter().any(
                            |(active_thread, active_host, active_device, active_epoch)| {
                                active_thread == &key.0
                                    && active_host == &key.1
                                    && active_device == &key.2
                                    && active_epoch == &status.session_epoch
                            },
                        );
                        (!keep).then(|| (key.clone(), (status.recording_id, status.session_epoch.clone())))
                    })
                    .collect::<Vec<_>>();
                self.recordings.retain(|(thread, host, device), status| {
                    active.iter().any(
                        |(active_thread, active_host, active_device, active_epoch)| {
                            active_thread == thread
                                && active_host == host
                                && active_device == device
                                && active_epoch == &status.session_epoch
                        },
                    )
                });
                for (key, lifetime) in removed_recordings {
                    self.closed_recordings.insert(key, lifetime);
                }
                if self.last_recording.as_ref().is_some_and(|recording| {
                    !active.iter().any(|(thread, host, device, epoch)| {
                        thread == &recording.status.thread_id.to_string()
                            && host == &recording.status.host_id
                            && device == &recording.status.device_id
                            && epoch == &recording.status.session_epoch
                    })
                }) {
                    self.last_recording = None;
                }
                self.clear_duo_for_closed_sessions();
                if frame_stream_changed {
                    self.frame_revision = self.frame_revision.saturating_add(1);
                }
                self.error = None;
            }
            DeviceEvent::Frame(frame) => {
                if !self.accepts_thread_event(
                    &frame.thread_id,
                    &frame.device.host_id,
                    &frame.device.id,
                    &frame.session_epoch,
                ) {
                    return;
                }
                self.frames.insert(
                    (
                        frame.thread_id.to_string(),
                        frame.device.host_id.clone(),
                        frame.device.id.clone(),
                    ),
                    frame,
                );
                self.frame_revision = self.frame_revision.saturating_add(1);
            }
            DeviceEvent::Video(frame) => {
                if !self.accepts_thread_event(
                    &frame.thread_id,
                    &frame.device.host_id,
                    &frame.device.id,
                    &frame.session_epoch,
                ) {
                    return;
                }
                let key = (
                    frame.thread_id.to_string(),
                    frame.device.host_id.clone(),
                    frame.device.id.clone(),
                    frame.screen_id.unwrap_or(0),
                );
                if self
                    .video_frames
                    .get(&key)
                    .is_some_and(|latest| latest.session_epoch != frame.session_epoch)
                {
                    self.video_frames.remove(&key);
                    self.video_events.remove(&key);
                }
                if self
                    .video_frames
                    .get(&key)
                    .is_some_and(|latest| frame.sequence <= latest.sequence)
                {
                    return;
                }
                self.video_frames.insert(key.clone(), frame.clone());
                let events = self.video_events.entry(key).or_default();
                events.push_back(frame);
                while events.len() > MAX_VIDEO_EVENTS_PER_STREAM {
                    events.pop_front();
                }
                self.frame_revision = self.frame_revision.saturating_add(1);
            }
            DeviceEvent::Accessibility(tree) => {
                if !self.accepts_device_event(&tree.host_id, &tree.device_id, &tree.session_epoch) {
                    return;
                }
                self.accessibility
                    .insert((tree.host_id.clone(), tree.device_id.clone()), tree);
            }
            DeviceEvent::EventLog(entry) => {
                if !self.accepts_device_event(
                    &entry.host_id,
                    &entry.device_id,
                    &entry.session_epoch,
                ) {
                    return;
                }
                let log = self
                    .event_log
                    .entry((entry.host_id.clone(), entry.device_id.clone()))
                    .or_default();
                if log
                    .first()
                    .is_some_and(|existing| existing.session_epoch != entry.session_epoch)
                {
                    log.clear();
                }
                if !log.iter().any(|existing| existing.id == entry.id) {
                    log.push(entry);
                    log.sort_by_key(|entry| entry.id);
                    if log.len() > 100 {
                        let keep_from = log.len() - 100;
                        log.drain(..keep_from);
                    }
                }
            }
            DeviceEvent::Foreground(update) => {
                if !self.accepts_device_event(
                    &update.host_id,
                    &update.device_id,
                    &update.session_epoch,
                ) {
                    return;
                }
                self.foreground
                    .insert((update.host_id.clone(), update.device_id.clone()), update);
            }
            DeviceEvent::Screen(screen) => {
                if let (Some(thread), Some(host), Some(device)) =
                    (&screen.thread_id, &screen.host_id, &screen.device_id)
                    && self.accepts_thread_event(thread, host, device, &screen.session_epoch)
                {
                    self.screens.insert(
                        (
                            thread.to_string(),
                            host.clone(),
                            device.clone(),
                            screen.screen_id.unwrap_or(0),
                        ),
                        screen,
                    );
                }
            }
            DeviceEvent::Recording(status) => {
                let key = (
                    status.thread_id.to_string(),
                    status.host_id.clone(),
                    status.device_id.clone(),
                );
                if status.active {
                    if !self.sessions.iter().any(|session| {
                        session.thread_id == status.thread_id
                            && session.host_id == status.host_id
                            && session.device_id == status.device_id
                            && session.session_epoch == status.session_epoch
                    }) {
                        return;
                    }
                    if self
                        .recordings
                        .get(&key)
                        .is_none_or(|current| status.recording_id >= current.recording_id)
                    {
                        self.closed_recordings.remove(&key);
                        self.recordings.insert(key, status);
                    }
                } else if self.recordings.get(&key).is_some_and(|current| {
                        current.recording_id == status.recording_id
                            && current.session_epoch == status.session_epoch
                    })
                {
                    self.recordings.remove(&key);
                    self.closed_recordings
                        .insert(key, (status.recording_id, status.session_epoch));
                }
            }
            DeviceEvent::RecordingComplete(recording) => {
                let key = (
                    recording.status.thread_id.to_string(),
                    recording.status.host_id.clone(),
                    recording.status.device_id.clone(),
                );
                let completion_was_pending = if let Some(current) = self.recordings.get(&key) {
                    if current.recording_id != recording.status.recording_id
                        || current.session_epoch != recording.status.session_epoch
                    {
                        return;
                    }
                    self.recordings.remove(&key);
                    false
                } else if self.sessions.iter().any(|session| {
                    session.thread_id.to_string() == key.0
                        && session.host_id == key.1
                        && session.device_id == key.2
                        && session.session_epoch != recording.status.session_epoch
                }) {
                    return;
                } else if self.closed_recordings.get(&key).is_none_or(
                    |(recording_id, session_epoch)| {
                        *recording_id != recording.status.recording_id
                            || session_epoch != &recording.status.session_epoch
                    },
                ) {
                    return;
                } else {
                    true
                };
                if completion_was_pending {
                    self.closed_recordings.remove(&key);
                }
                if self.last_recording.as_ref().is_none_or(|current| {
                    let same_lifetime_key = current.status.thread_id == recording.status.thread_id
                        && current.status.host_id == recording.status.host_id
                        && current.status.device_id == recording.status.device_id;
                    !same_lifetime_key
                        || recording.status.recording_id >= current.status.recording_id
                }) {
                    self.last_recording = Some(recording);
                }
            }
        }
    }

    pub fn service(&self) -> DeviceServiceState {
        self.service.clone().unwrap_or_default()
    }

    pub fn session(&self, thread: &str) -> Option<&DeviceSession> {
        self.sessions
            .iter()
            .rev()
            .find(|session| session.thread_id.as_str() == thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::device::{
        DeviceAccessibilityTree, DeviceEventLogEntry, DeviceForegroundUpdate, DeviceFrame,
        DeviceFrameEncoding, DevicePlatform, DeviceRecording, DeviceRecordingFormat,
        DeviceRecordingStatus, DeviceScreenConfig, DeviceSummary, DeviceVideoFrame,
    };

    fn session(thread: &str, host: &str, device: &str) -> DeviceSession {
        DeviceSession {
            thread_id: agent_domain::ThreadId::new(thread).unwrap(),
            host_id: host.into(),
            device_id: device.into(),
            platform: DevicePlatform::Android,
            opened_at: "0".into(),
            session_epoch: "0".into(),
        }
    }

    #[test]
    fn state_updates_prune_frames_for_closed_sessions() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Frame(DeviceFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: current.session_epoch.clone(),
            device: DeviceSummary {
                host_id: current.host_id.clone(),
                id: current.device_id.clone(),
                platform: current.platform,
                name: "Pixel".into(),
                version: "Android".into(),
                booted: true,
                physical: false,
            },
            png: vec![1],
            width: 1,
            height: 1,
            sequence: 1,
        }));
        assert_eq!(state.frames.len(), 1);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.frames.is_empty());
        state.details.insert(
            (current.host_id.clone(), current.device_id.clone()),
            DeviceDetail {
                host_id: current.host_id,
                device_id: current.device_id,
                settings: Default::default(),
                foreground_app: None,
                read_at: "0".into(),
            },
        );
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.details.is_empty());
    }

    #[test]
    fn ordered_video_events_advance_frame_revision_and_reject_stale_epochs() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let frame = |epoch: &str, sequence: u64| DeviceVideoFrame {
            thread_id: current.thread_id.clone(),
            session_epoch: epoch.into(),
            device: DeviceSummary {
                host_id: current.host_id.clone(),
                id: current.device_id.clone(),
                platform: current.platform,
                name: "Pixel".into(),
                version: "Android".into(),
                booted: true,
                physical: false,
            },
            payload: vec![0, 0, 0, 1, 0x65],
            encoding: DeviceFrameEncoding::H264,
            width: 100,
            height: 200,
            sequence,
            timestamp_us: Some(sequence),
            keyframe: sequence == 1,
            screen_id: Some(1),
        };
        state.apply_event(DeviceEvent::Video(frame("0", 1)));
        state.apply_event(DeviceEvent::Video(frame("0", 2)));
        state.apply_event(DeviceEvent::Video(frame("0", 2)));
        assert_eq!(state.frame_revision, 2);
        assert_eq!(state.video_events.values().next().unwrap().len(), 2);
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        state.apply_event(DeviceEvent::Video(frame("0", 3)));
        assert!(state.video_events.is_empty());
        assert_eq!(state.frame_revision, 3);

        let mut reopened = current.clone();
        reopened.session_epoch = "new".into();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Video(frame("new", 1)));
        assert_eq!(
            state.video_frames.values().next().unwrap().session_epoch,
            "new"
        );
        assert_eq!(state.video_events.values().next().unwrap().len(), 1);
    }

    #[test]
    fn canonical_keyboard_facts_preserve_source_semantics_and_physical_code() {
        assert_eq!(
            canonical_device_key("", "a"),
            DeviceKeyFacts {
                code: "KeyA".into(),
                key: "a".into()
            }
        );
        assert_eq!(
            canonical_device_key("", "!"),
            DeviceKeyFacts {
                code: "Digit1".into(),
                key: "!".into()
            }
        );
        assert_eq!(
            canonical_device_key("KeyA", "ä"),
            DeviceKeyFacts {
                code: "KeyA".into(),
                key: "ä".into()
            }
        );
        assert_eq!(
            canonical_device_key("", "up"),
            DeviceKeyFacts {
                code: "ArrowUp".into(),
                key: "ArrowUp".into()
            }
        );
        assert_eq!(
            canonical_device_key("", "Space"),
            DeviceKeyFacts {
                code: "Space".into(),
                key: " ".into()
            }
        );
        assert_eq!(
            canonical_device_key("shift", ""),
            DeviceKeyFacts {
                code: "ShiftLeft".into(),
                key: "".into()
            }
        );
        assert_eq!(
            canonical_device_key("ArrowUp", "up"),
            DeviceKeyFacts {
                code: "ArrowUp".into(),
                key: "ArrowUp".into()
            }
        );
    }

    #[test]
    fn modifier_transitions_cover_shifted_key_and_release_cleanup() {
        let empty = DeviceModifierFacts::default();
        let shifted = DeviceModifierFacts {
            shift: true,
            ..empty
        };
        assert_eq!(
            device_modifier_transitions(empty, shifted),
            vec![DeviceModifierTransition {
                code: "ShiftLeft".into(),
                down: true,
            }]
        );
        assert_eq!(
            device_modifier_transitions(shifted, empty),
            vec![DeviceModifierTransition {
                code: "ShiftLeft".into(),
                down: false,
            }]
        );
    }

    #[test]
    fn modifier_transitions_release_in_reverse_order_for_surface_reset() {
        let all = DeviceModifierFacts {
            shift: true,
            alt: true,
            meta: true,
            ctrl: true,
        };
        let transitions = device_modifier_transitions(all, DeviceModifierFacts::default());
        assert_eq!(
            transitions
                .into_iter()
                .map(|transition| (transition.code, transition.down))
                .collect::<Vec<_>>(),
            vec![
                ("MetaLeft".into(), false),
                ("AltLeft".into(), false),
                ("ShiftLeft".into(), false),
                ("ControlLeft".into(), false),
            ]
        );
    }

    #[test]
    fn native_input_plan_serializes_modifiers_and_releases_all_pressed_keys() {
        let mut current = session("thread", "host", "device");
        current.platform = DevicePlatform::Ios;
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let shifted = DeviceModifierFacts {
            shift: true,
            ..DeviceModifierFacts::default()
        };
        let plan = state
            .key_input_plan(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                "KeyA".into(),
                "A".into(),
                current.session_epoch.clone(),
                true,
                shifted,
            )
            .unwrap();
        assert!(plan.inputs.iter().all(|input| {
            input.thread_id == current.thread_id
                && input.host_id.as_deref() == Some(current.host_id.as_str())
                && input.device_id == current.device_id
                && input.session_epoch == current.session_epoch
        }));
        let codes = plan
            .inputs
            .iter()
            .map(|input| match &input.input {
                agent_protocol::device::DeviceInputKind::Key { code, down, .. } => {
                    (code.clone(), *down)
                }
                _ => panic!("key plan contains a non-key input"),
            })
            .collect::<Vec<_>>();
        assert_eq!(codes, vec![("ShiftLeft".into(), true), ("KeyA".into(), true)]);

        assert_eq!(
            state
                .release_input_plan(
                    current.thread_id.clone(),
                    Some(current.host_id.clone()),
                    current.device_id.clone(),
                    Some("stale-epoch".into()),
                )
                .unwrap_err(),
            "device session is not open"
        );

        let cleanup = state
            .release_input_plan(
                current.thread_id,
                Some(current.host_id),
                current.device_id,
                Some(current.session_epoch),
            )
            .unwrap()
            .unwrap();
        let cleanup_codes = cleanup
            .inputs
            .iter()
            .map(|input| match &input.input {
                agent_protocol::device::DeviceInputKind::Key { code, down, .. } => {
                    (code.clone(), *down)
                }
                _ => panic!("cleanup plan contains a non-key input"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            cleanup_codes,
            vec![("ShiftLeft".into(), false), ("KeyA".into(), false)]
        );
    }

    #[test]
    fn duo_queue_is_single_flight_and_clears_on_reconnect() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let first = state
            .enqueue_duo(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                crate::state::DeviceDuoCommandIntent::Angle { value: 30.0 },
            )
            .unwrap()
            .unwrap();
        assert!(
            state
                .enqueue_duo(
                    current.thread_id.clone(),
                    Some(current.host_id.clone()),
                    current.device_id.clone(),
                    crate::state::DeviceDuoCommandIntent::Angle { value: 60.0 },
                )
                .unwrap()
                .is_none()
        );
        let next = state.complete_duo(&first, true, None).unwrap();
        assert_eq!(
            next.command,
            crate::state::DeviceDuoCommandIntent::Angle { value: 60.0 }
        );
        // A late completion for the first request cannot settle the promoted
        // request, even though both commands target the same device.
        assert!(state.complete_duo(&first, true, None).is_none());
        state.fail_duo(&next, "Duo control failed");
        assert_eq!(
            state
                .duo_controls
                .values()
                .next()
                .and_then(|control| control.error.as_deref()),
            Some("Duo control failed")
        );
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.duo_controls.is_empty());
    }

    #[test]
    fn promoted_duo_is_rejected_after_the_session_epoch_changes() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let request = state
            .enqueue_duo(
                current.thread_id.clone(),
                Some(current.host_id.clone()),
                current.device_id.clone(),
                crate::state::DeviceDuoCommandIntent::Angle { value: 60.0 },
            )
            .unwrap()
            .unwrap();
        state.sessions[0].session_epoch = "reconnected".into();
        assert!(!state.accepts_duo_request(&request));
        // A queued command must never be sent to a replacement session that
        // reused the same device id.
        assert!(state.complete_duo(&request, true, None).is_none());
        assert!(state.duo_controls.is_empty());
    }

    #[test]
    fn recording_events_keep_active_state_and_release_completed_bytes() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let status = DeviceRecordingStatus {
            thread_id: current.thread_id.clone(),
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            recording_id: 1,
            session_epoch: current.session_epoch.clone(),
            format: DeviceRecordingFormat::Mp4,
            file_name: "device.mp4".into(),
            mime_type: "video/mp4".into(),
            active: true,
            started_at: "0".into(),
            frame_count: 1,
            byte_count: 2,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(status.clone()));
        assert_eq!(state.recordings.len(), 1);
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus {
                active: false,
                ..status
            },
            bytes: vec![1, 2],
        }));
        assert!(state.recordings.is_empty());
        assert_eq!(state.last_recording.as_ref().unwrap().bytes, vec![1, 2]);
        let reopened = DeviceSession {
            session_epoch: "new".into(),
            ..current
        };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened],
            ..DeviceServiceState::default()
        }));
        assert!(state.last_recording.is_none());
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        assert!(state.recordings.is_empty());
        assert!(state.last_recording.is_none());
    }

    #[test]
    fn reopening_a_session_prunes_and_rejects_old_device_metadata() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Accessibility(DeviceAccessibilityTree {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            elements: vec![],
            errors: vec![],
            read_at: "old".into(),
        }));
        state.apply_event(DeviceEvent::Foreground(DeviceForegroundUpdate {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            app: None,
            received_at: "old".into(),
        }));
        state.apply_event(DeviceEvent::EventLog(DeviceEventLogEntry {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            id: 1,
            timestamp: "old".into(),
            kind: "old".into(),
            summary: "old".into(),
        }));
        state.apply_event(DeviceEvent::Screen(DeviceScreenConfig {
            thread_id: Some(current.thread_id.clone()),
            session_epoch: current.session_epoch.clone(),
            host_id: Some(current.host_id.clone()),
            device_id: Some(current.device_id.clone()),
            width: 1,
            height: 1,
            orientation: agent_protocol::device::DeviceOrientation::Portrait,
            screen_id: None,
            supports_hinge_angle: false,
            supports_physical_orientation: false,
            hinge_angle: None,
            hinge_pose: None,
            table_mode: false,
            table_mode_available: false,
        }));

        let reopened = DeviceSession {
            session_epoch: "new".into(),
            ..current.clone()
        };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![reopened.clone()],
            ..DeviceServiceState::default()
        }));
        assert!(state.accessibility.is_empty());
        assert!(state.foreground.is_empty());
        assert!(state.event_log.is_empty());
        assert!(state.screens.is_empty());

        state.apply_event(DeviceEvent::Accessibility(DeviceAccessibilityTree {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch.clone(),
            elements: vec![],
            errors: vec![],
            read_at: "stale".into(),
        }));
        state.apply_event(DeviceEvent::EventLog(DeviceEventLogEntry {
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            session_epoch: current.session_epoch,
            id: 2,
            timestamp: "stale".into(),
            kind: "stale".into(),
            summary: "stale".into(),
        }));
        assert!(state.accessibility.is_empty());
        assert!(state.event_log.is_empty());
        assert!(state.accepts_device_event(
            &reopened.host_id,
            &reopened.device_id,
            &reopened.session_epoch
        ));
    }

    #[test]
    fn recording_completion_survives_session_removal() {
        let current = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        let status = DeviceRecordingStatus {
            thread_id: current.thread_id.clone(),
            host_id: current.host_id.clone(),
            device_id: current.device_id.clone(),
            recording_id: 1,
            session_epoch: current.session_epoch.clone(),
            format: DeviceRecordingFormat::Mp4,
            file_name: "device.mp4".into(),
            mime_type: "video/mp4".into(),
            active: true,
            started_at: "0".into(),
            frame_count: 1,
            byte_count: 2,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(status.clone()));
        state.apply_event(DeviceEvent::State(DeviceServiceState::default()));
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus {
                active: false,
                ..status
            },
            bytes: vec![1, 2],
        }));
        assert_eq!(state.last_recording.as_ref().unwrap().bytes, vec![1, 2]);
    }

    #[test]
    fn late_completion_cannot_remove_a_new_recording_lifetime() {
        let old = session("thread", "host", "device");
        let mut state = DeviceState::default();
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![old.clone()],
            ..DeviceServiceState::default()
        }));
        let old_status = DeviceRecordingStatus {
            thread_id: old.thread_id.clone(),
            host_id: old.host_id.clone(),
            device_id: old.device_id.clone(),
            recording_id: 1,
            session_epoch: old.session_epoch.clone(),
            format: DeviceRecordingFormat::Mp4,
            file_name: "old.mp4".into(),
            mime_type: "video/mp4".into(),
            active: true,
            started_at: "old".into(),
            frame_count: 1,
            byte_count: 1,
            error: None,
        };
        state.apply_event(DeviceEvent::Recording(old_status.clone()));

        let current = DeviceSession {
            session_epoch: "new".into(),
            ..old.clone()
        };
        state.apply_event(DeviceEvent::State(DeviceServiceState {
            sessions: vec![current.clone()],
            ..DeviceServiceState::default()
        }));
        state.apply_event(DeviceEvent::Recording(old_status.clone()));
        assert!(state.recordings.is_empty());
        let new_status = DeviceRecordingStatus {
            recording_id: 2,
            session_epoch: current.session_epoch.clone(),
            file_name: "new.mp4".into(),
            ..old_status.clone()
        };
        state.apply_event(DeviceEvent::Recording(new_status.clone()));
        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus {
                active: false,
                ..old_status
            },
            bytes: vec![1],
        }));
        assert_eq!(
            state
                .recordings
                .get(&("thread".into(), "host".into(), "device".into()))
                .map(|status| status.recording_id),
            Some(2)
        );
        assert!(state.last_recording.is_none());

        state.apply_event(DeviceEvent::RecordingComplete(DeviceRecording {
            status: DeviceRecordingStatus {
                active: false,
                ..new_status
            },
            bytes: vec![2],
        }));
        assert!(state.recordings.is_empty());
        assert_eq!(
            state
                .last_recording
                .as_ref()
                .map(|recording| recording.bytes.clone()),
            Some(vec![2])
        );
    }
}
