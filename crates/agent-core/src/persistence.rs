//! Device-owned state core keeps in one file per Host: drafts, navigation,
//! commands the Host has not confirmed, and the last Host identity needed to
//! render environment views while offline. Client preferences are shared by
//! every Host and are stored separately. Conversation data lives in the disk
//! cache.
use crate::commands::{build::FollowUpBehavior, outbox::Outbox};
use crate::models::EnvironmentDescriptor;
use crate::state::{Draft, PendingRollback, Preferences, Shared, Snapshot};
use crate::view::composer::stash::PromptStash;
use agent_domain::{CommandId, ThreadId};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    io::Write,
    path::Path,
    sync::Arc,
};

/// How long a change other than a new outbox entry waits before it is written.
pub const STATE_WRITE_DELAY_MS: u64 = 250;
/// A failed write is tried again after this.
pub const STATE_RETRY_MS: u64 = 1_000;

pub(crate) const MODEL_PREFERENCES_RECOVERY_ERROR: &str =
    "Saved model preferences could not be read. Choose a model again.";

#[derive(Default, Serialize, Deserialize)]
struct LocalState {
    drafts: BTreeMap<String, Draft>,
    follow_up: FollowUpBehavior,
    selected_thread: Option<ThreadId>,
    selected_project: Option<String>,
    open_new_thread_draft: Option<String>,
    outbox: Outbox,
    rollbacks: BTreeMap<CommandId, PendingRollback>,
    /// These values describe the Host/project state that must remain scoped
    /// to this device-state file. Client-global preferences are stored in the
    /// separate model-preferences payload.
    last_run_scripts: BTreeMap<String, String>,
    resume_compaction_dismissed: BTreeSet<String>,
    stash: PromptStash,
    /// The last authenticated Host identity, retained for offline environment views.
    #[serde(default)]
    environment: Option<EnvironmentDescriptor>,
}

pub(crate) fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&LocalState {
        drafts: (*snapshot.drafts).clone(),
        follow_up: snapshot.follow_up,
        selected_thread: snapshot.selected_thread.clone(),
        selected_project: snapshot.selected_project.clone(),
        open_new_thread_draft: snapshot.open_new_thread_draft.clone(),
        outbox: snapshot.outbox.persisted(),
        rollbacks: snapshot.rollbacks.clone(),
        last_run_scripts: snapshot.preferences.last_run_scripts.clone(),
        resume_compaction_dismissed: snapshot.preferences.resume_compaction_dismissed.clone(),
        stash: (*snapshot.stash).clone(),
        environment: snapshot.environment.clone(),
    })
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Snapshot, serde_json::Error> {
    let local: LocalState = if bytes.is_empty() {
        LocalState::default()
    } else {
        serde_json::from_slice(bytes)?
    };
    let preferences = Preferences {
        last_run_scripts: local.last_run_scripts,
        resume_compaction_dismissed: local.resume_compaction_dismissed,
        ..Default::default()
    };
    Ok(Snapshot {
        drafts: local.drafts.into(),
        default_draft: Snapshot::default().default_draft,
        follow_up: local.follow_up,
        selected_thread: local.selected_thread,
        selected_project: local.selected_project,
        open_new_thread_draft: local.open_new_thread_draft,
        outbox: Arc::new(local.outbox),
        rollbacks: local.rollbacks,
        preferences,
        stash: local.stash.into(),
        environment: local.environment,
        ..Snapshot::default()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClientPreferences {
    default_draft: Draft,
    preferences: Preferences,
}

/// Serializes the client-global preferences shared by every Host store.
/// Host/project state is deliberately excluded from this payload.
pub fn encode_model_preferences(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&ClientPreferences {
        default_draft: snapshot.default_draft.user_defaults(),
        preferences: snapshot.preferences.clone(),
    })
}

/// Returns the current canonical client-preferences payload for bytes loaded
/// from device storage. A damaged payload becomes the fresh default payload;
/// the seeded snapshot still carries `MODEL_PREFERENCES_RECOVERY_ERROR` for
/// the owner to show before it replaces the stored bytes.
pub fn canonical_model_preferences(bytes: &[u8]) -> Vec<u8> {
    let snapshot = Snapshot::with_client_preferences(bytes);
    encode_model_preferences(&snapshot).unwrap_or_default()
}

/// Applies an explicit client-global preference payload while retaining the
/// local Host/project values already held by `snapshot`.
pub fn apply_model_preferences(
    snapshot: &mut Snapshot,
    bytes: &[u8],
) -> Result<(), serde_json::Error> {
    let client: ClientPreferences = serde_json::from_slice(bytes)?;
    let last_run_scripts = std::mem::take(&mut snapshot.preferences.last_run_scripts);
    let resume_compaction_dismissed =
        std::mem::take(&mut snapshot.preferences.resume_compaction_dismissed);
    snapshot.default_draft = client.default_draft.user_defaults();
    snapshot.preferences = client.preferences;
    snapshot.preferences.last_run_scripts = last_run_scripts;
    snapshot.preferences.resume_compaction_dismissed = resume_compaction_dismissed;
    Ok(())
}

/// The device state saved at `path` with the client preferences every Host
/// shares; a missing file is a new device.
pub fn load(path: &Path, client_preferences: &[u8]) -> Snapshot {
    let saved = match fs::read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(vec![]),
        read => read,
    };
    recover(saved.ok().as_deref(), client_preferences)
}

/// Replaces the device state file once the new bytes reach storage.
pub fn save(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    write_file(path, &encode(snapshot)?)
}

/// Replaces `path` once the new bytes reach storage, readable by the user only.
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("A saved file needs a directory."))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Decodes independent device-owned components; a damaged or unreadable file
/// must not lock out a Host.
fn recover(saved: Option<&[u8]>, client_preferences: &[u8]) -> Snapshot {
    let mut state = saved
        .and_then(|bytes| decode(bytes).ok())
        .unwrap_or_else(|| Snapshot {
            follow_up: FollowUpBehavior::default(),
            error: Some("Saved device state could not be read. Device drafts were reset.".into()),
            ..Default::default()
        });
    if !client_preferences.is_empty() {
        match apply_model_preferences(&mut state, client_preferences) {
            Ok(()) => {}
            Err(_) => state.error = Some(MODEL_PREFERENCES_RECOVERY_ERROR.into()),
        }
    }
    for draft in state.drafts.values_mut() {
        for attachment in &mut draft.attachments {
            if attachment.status == "uploading" {
                attachment.status = "failed".into();
                attachment.error = Some("Upload interrupted. Retry to continue.".into());
            }
        }
    }
    state.stash.settle_pending_images();
    state
}

/// The saved parts of a published state: shared storage by identity, the
/// rest by value.
struct Saved {
    drafts: Shared<BTreeMap<String, Draft>>,
    outbox: Arc<Outbox>,
    stash: Shared<PromptStash>,
    follow_up: FollowUpBehavior,
    selected_thread: Option<ThreadId>,
    selected_project: Option<String>,
    open_new_thread_draft: Option<String>,
    rollbacks: BTreeMap<CommandId, PendingRollback>,
    last_run_scripts: BTreeMap<String, String>,
    resume_compaction_dismissed: BTreeSet<String>,
    environment: Option<EnvironmentDescriptor>,
}
impl Saved {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            drafts: snapshot.drafts.clone(),
            outbox: snapshot.outbox.clone(),
            stash: snapshot.stash.clone(),
            follow_up: snapshot.follow_up,
            selected_thread: snapshot.selected_thread.clone(),
            selected_project: snapshot.selected_project.clone(),
            open_new_thread_draft: snapshot.open_new_thread_draft.clone(),
            rollbacks: snapshot.rollbacks.clone(),
            last_run_scripts: snapshot.preferences.last_run_scripts.clone(),
            resume_compaction_dismissed: snapshot.preferences.resume_compaction_dismissed.clone(),
            environment: snapshot.environment.clone(),
        }
    }
    fn same(&self, other: &Self) -> bool {
        self.drafts.shares_storage(&other.drafts)
            && Arc::ptr_eq(&self.outbox, &other.outbox)
            && self.stash.shares_storage(&other.stash)
            && self.follow_up == other.follow_up
            && self.selected_thread == other.selected_thread
            && self.selected_project == other.selected_project
            && self.open_new_thread_draft == other.open_new_thread_draft
            && self.rollbacks == other.rollbacks
            && self.last_run_scripts == other.last_run_scripts
            && self.resume_compaction_dismissed == other.resume_compaction_dismissed
            && self.environment == other.environment
    }
}

/// When the device state is written. A new outbox entry is written at once
/// and is sent only once a write holds it; other changes wait 250 ms.
#[derive(Default)]
pub struct StateWriter {
    observed: Option<Saved>,
    due_at: Option<u64>,
    changed: bool,
    /// The outbox entries the write in progress holds.
    writing: Option<BTreeSet<CommandId>>,
    /// The outbox entries the last stored write holds.
    durable: BTreeSet<CommandId>,
}
impl StateWriter {
    /// For state read from its file, whose outbox entries are stored.
    pub fn restored(snapshot: &Snapshot) -> Self {
        Self {
            observed: Some(Saved::of(snapshot)),
            durable: snapshot
                .outbox
                .entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect(),
            ..Self::default()
        }
    }

    /// Notes a published state.
    pub fn observe(&mut self, snapshot: &Snapshot, now: u64) {
        let saved = Saved::of(snapshot);
        if self
            .observed
            .as_ref()
            .is_some_and(|observed| observed.same(&saved))
        {
            return;
        }
        self.observed = Some(saved);
        self.changed = true;
        let unstored = snapshot.outbox.entries.iter().any(|entry| {
            !self.durable.contains(&entry.id)
                && !self
                    .writing
                    .as_ref()
                    .is_some_and(|writing| writing.contains(&entry.id))
        });
        let at = if unstored {
            now
        } else {
            now + STATE_WRITE_DELAY_MS
        };
        self.due_at = Some(self.due_at.map_or(at, |due| due.min(at)));
    }

    pub fn next_due(&self) -> Option<u64> {
        self.due_at.filter(|_| self.writing.is_none())
    }

    /// Whether a write of `snapshot` is due; it starts if so.
    pub fn due(&mut self, snapshot: &Snapshot, now: u64) -> bool {
        if self.writing.is_some() || self.due_at.is_none_or(|due| due > now) {
            return false;
        }
        self.due_at = None;
        self.changed = false;
        self.writing = Some(
            snapshot
                .outbox
                .entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect(),
        );
        true
    }

    /// A write ended; a failed one is tried again.
    pub fn written(&mut self, stored: bool, now: u64) {
        let entries = self.writing.take().unwrap_or_default();
        if stored {
            self.durable = entries;
        } else {
            self.changed = true;
            let retry = now + STATE_RETRY_MS;
            self.due_at = Some(self.due_at.map_or(retry, |due| due.max(retry)));
        }
    }

    /// Whether a write holds this outbox entry.
    pub fn stored(&self, id: &CommandId) -> bool {
        self.durable.contains(id)
    }

    /// Whether the latest state is written or being written.
    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn writing(&self) -> bool {
        self.writing.is_some()
    }

    /// Writes the latest state now.
    pub fn hurry(&mut self, now: u64) {
        if self.changed {
            self.due_at = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{build::dispatch, outbox::*};

    #[test]
    fn damaged_components_do_not_discard_valid_device_work_or_block_startup() {
        let mut state = Snapshot::default();
        state.default_draft.model = "valid model".into();
        state.drafts.insert(
            "thread".into(),
            Draft {
                text: "keep me".into(),
                ..Default::default()
            },
        );
        let recovered = recover(Some(&encode(&state).unwrap()), b"broken preferences");
        assert_eq!(recovered.drafts["thread"].text, "keep me");
        assert_ne!(recovered.default_draft.model, "valid model");
        assert!(recovered.error.is_some());
        let preferences = encode_model_preferences(&state).unwrap();
        let recovered = recover(Some(b"broken state"), &preferences);
        assert_eq!(recovered.default_draft.model, "valid model");
        assert!(recovered.error.is_some());
        let directory = tempfile::tempdir().unwrap();
        let unreadable = load(directory.path(), &preferences);
        assert_eq!(unreadable.default_draft.model, "valid model");
        assert!(unreadable.error.is_some());
    }

    #[test]
    fn default_snapshot_seeding_uses_current_preferences_without_io() {
        let mut source = Snapshot::default();
        source.default_draft.model = "current model".into();
        source.preferences.notification_mode =
            crate::view::notifications::NotificationMode::Notifications;
        let bytes = encode_model_preferences(&source).unwrap();

        let seeded = Snapshot::with_client_preferences(&bytes);
        assert_eq!(seeded.default_draft.model, "current model");
        assert_eq!(
            seeded.preferences.notification_mode,
            crate::view::notifications::NotificationMode::Notifications
        );
        assert!(seeded.drafts.is_empty());
        assert!(seeded.error.is_none());

        let fresh = Snapshot::with_client_preferences(&[]);
        assert_eq!(fresh.default_draft, Snapshot::default().default_draft);
        assert!(fresh.error.is_none());
    }

    #[test]
    fn malformed_default_preferences_keep_fresh_state_and_recovery_notice() {
        let seeded = Snapshot::with_client_preferences(b"malformed preferences");
        assert_eq!(seeded.default_draft, Snapshot::default().default_draft);
        assert_eq!(
            seeded.error.as_deref(),
            Some(MODEL_PREFERENCES_RECOVERY_ERROR)
        );
    }

    #[test]
    fn malformed_client_preferences_are_replaced_by_canonical_defaults() {
        let bytes = canonical_model_preferences(b"malformed preferences");
        let mut restored = Snapshot::default();
        apply_model_preferences(&mut restored, &bytes).unwrap();
        assert_eq!(restored.default_draft, Snapshot::default().default_draft);
        assert!(restored.error.is_none());
    }

    #[test]
    fn client_preferences_are_shared_without_replacing_host_local_state() {
        let mut first = Snapshot::default();
        first.default_draft.model = "shared-model".into();
        first.preferences.notification_mode =
            crate::view::notifications::NotificationMode::Notifications;
        first
            .preferences
            .last_run_scripts
            .insert("project-a".into(), "script-a".into());
        let bytes = encode_model_preferences(&first).unwrap();

        let mut second = Snapshot::default();
        second.default_draft.model = "stale-model".into();
        second
            .preferences
            .last_run_scripts
            .insert("project-b".into(), "script-b".into());
        second
            .preferences
            .resume_compaction_dismissed
            .insert("claude".into());
        apply_model_preferences(&mut second, &bytes).unwrap();

        assert_eq!(second.default_draft.model, "shared-model");
        assert_eq!(
            second.preferences.notification_mode,
            crate::view::notifications::NotificationMode::Notifications
        );
        assert_eq!(
            second.preferences.last_run_scripts.get("project-b"),
            Some(&"script-b".to_owned())
        );
        assert!(
            second
                .preferences
                .resume_compaction_dismissed
                .contains("claude")
        );
        assert!(
            !second
                .preferences
                .last_run_scripts
                .contains_key("project-a")
        );
    }

    #[test]
    fn host_state_does_not_replicate_client_preferences() {
        let mut state = Snapshot::default();
        state.preferences.notification_mode =
            crate::view::notifications::NotificationMode::Notifications;
        state
            .preferences
            .last_run_scripts
            .insert("project".into(), "script".into());

        let local = String::from_utf8(encode(&state).unwrap()).unwrap();
        assert!(local.contains("last_run_scripts"));
        assert!(!local.contains("notification_mode"));
        assert!(!local.contains("default_draft"));

        let shared = String::from_utf8(encode_model_preferences(&state).unwrap()).unwrap();
        assert!(shared.contains("notification_mode"));
        assert!(!shared.contains("last_run_scripts"));
    }

    #[test]
    fn a_saved_state_loads_back_and_a_missing_file_is_a_new_device() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host").join("device.json");
        let new = load(&path, &[]);
        assert_eq!(new.error, None);
        assert!(new.drafts.is_empty());
        let mut state = Snapshot::default();
        state.drafts.insert(
            "thread".into(),
            Draft {
                text: "saved".into(),
                ..Default::default()
            },
        );
        save(&path, &state).unwrap();
        state.drafts.insert("thread".into(), Draft::default());
        save(&path, &state).unwrap();
        assert_eq!(load(&path, &[]).drafts["thread"].text, "");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    fn environment_descriptor() -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            environment_id: "environment-id".into(),
            label: "Offline Host".into(),
            cwd: "/workspace/project".into(),
            platform: crate::models::EnvironmentPlatform {
                os: "linux".into(),
                arch: "x64".into(),
                machine: Some("desktop".into()),
            },
            server_version: "1.2.3".into(),
            orchestration_protocol_version: Some(2),
            capabilities: crate::models::EnvironmentCapabilities {
                usage_limit_sources: true,
                environment_icon: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn environment_identity_roundtrips_for_offline_views() {
        let state = Snapshot {
            environment: Some(environment_descriptor()),
            ..Default::default()
        };

        let restored = decode(&encode(&state).unwrap()).unwrap();

        assert_eq!(restored.environment, state.environment);
        let mut registry = crate::environment::EnvironmentRegistry::default();
        assert_eq!(
            registry.update(Arc::new(restored)),
            Some("environment-id".into())
        );
        assert_eq!(registry.summaries()[0].descriptor, environment_descriptor());
    }

    fn queued(id: &str) -> PendingCommand {
        let thread = ThreadId::new("thread").unwrap();
        PendingCommand::new(
            thread.clone(),
            Request::Dispatch(Box::new(dispatch(
                thread,
                CommandId::new(id).unwrap(),
                agent_domain::Command::MarkUnread,
            ))),
            agent_domain::Timestamp::from_millis(0).unwrap(),
        )
    }

    #[test]
    fn a_new_outbox_entry_is_written_at_once_and_stored_once_the_write_ends() {
        let mut state = Snapshot::default();
        let mut writer = StateWriter::restored(&state);
        writer.observe(&state, 0);
        assert_eq!(writer.next_due(), None);
        Arc::make_mut(&mut state.outbox)
            .enqueue(queued("command"))
            .unwrap();
        writer.observe(&state, 10);
        assert_eq!(writer.next_due(), Some(10));
        let id = CommandId::new("command").unwrap();
        assert!(writer.due(&state, 10));
        assert!(!writer.stored(&id));
        // A draft typed during the write waits for it, then 250 ms.
        state.drafts.insert("thread".into(), Draft::default());
        writer.observe(&state, 20);
        assert_eq!(writer.next_due(), None);
        assert!(!writer.due(&state, 300));
        writer.written(true, 30);
        assert!(writer.stored(&id));
        assert_eq!(writer.next_due(), Some(20 + STATE_WRITE_DELAY_MS));
    }

    #[test]
    fn a_failed_write_is_tried_again_and_restored_entries_are_stored() {
        let mut state = Snapshot::default();
        Arc::make_mut(&mut state.outbox)
            .enqueue(queued("restored"))
            .unwrap();
        let mut writer = StateWriter::restored(&state);
        assert!(writer.stored(&CommandId::new("restored").unwrap()));
        state.follow_up = FollowUpBehavior::Steer;
        writer.observe(&state, 0);
        assert!(writer.due(&state, STATE_WRITE_DELAY_MS));
        writer.written(false, 300);
        assert!(writer.changed());
        assert_eq!(writer.next_due(), Some(300 + STATE_RETRY_MS));
    }

    #[test]
    fn environment_identity_changes_schedule_a_device_state_write() {
        let mut state = Snapshot::default();
        let mut writer = StateWriter::restored(&state);
        writer.observe(&state, 0);
        assert_eq!(writer.next_due(), None);

        state.environment = Some(environment_descriptor());
        writer.observe(&state, 100);

        assert_eq!(writer.next_due(), Some(100 + STATE_WRITE_DELAY_MS));
    }

    #[test]
    fn drafts_settings_and_unconfirmed_commands_roundtrip_without_host_data() {
        let mut snapshot = Snapshot {
            connected: true,
            host_name: Some("Host".into()),
            follow_up: FollowUpBehavior::Steer,
            ..Snapshot::default()
        };
        snapshot.drafts.insert(
            "new:chats".into(),
            Draft {
                text: "Unsent message".into(),
                project_id: Some("chats".into()),
                ..Draft::default()
            },
        );
        let thread = ThreadId::new("thread").unwrap();
        let id = CommandId::new("command").unwrap();
        let mut outbox = Outbox::default();
        outbox
            .enqueue(PendingCommand::new(
                thread.clone(),
                Request::Dispatch(Box::new(dispatch(
                    thread,
                    id.clone(),
                    agent_domain::Command::MarkUnread,
                ))),
                agent_domain::Timestamp::from_millis(0).unwrap(),
            ))
            .unwrap();
        outbox.sending(&id);
        snapshot.outbox = Arc::new(outbox);
        let restored = decode(&encode(&snapshot).unwrap()).unwrap();
        assert!(!restored.connected);
        assert!(restored.host_name.is_none());
        assert_eq!(restored.drafts["new:chats"].text, "Unsent message");
        assert_eq!(restored.follow_up, FollowUpBehavior::Steer);
        assert!(restored.shell.snapshot.is_none());
        assert_eq!(restored.outbox.entries[0].phase, Phase::Queued);
    }

    #[test]
    fn a_new_device_defaults_to_queueing_follow_ups() {
        assert_eq!(decode(&[]).unwrap().follow_up, FollowUpBehavior::Queue);
    }

    #[test]
    fn global_client_preference_changes_do_not_schedule_host_state_writes() {
        let state = Snapshot::default();
        let mut writer = StateWriter::restored(&state);
        writer.observe(&state, 0);
        let mut changed = state.clone();
        changed.preferences.notification_mode =
            crate::view::notifications::NotificationMode::Notifications;
        writer.observe(&changed, 1);
        assert_eq!(writer.next_due(), None);
        assert!(!writer.changed());
    }
}
