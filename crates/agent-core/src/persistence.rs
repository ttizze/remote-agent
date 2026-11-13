//! Device storage is an explicit subset of runtime state. Host results and
//! connection authority are fetched again, never restored from this format.
//!
//! Desktop, Swift and Kotlin use this codec; Snapshot inspection JSON is not a
//! storage format. Preserve user-owned drafts, attachments, pending sends, edits,
//! preferences and navigation across Host storage scopes. Reject damaged required
//! fields rather than silently replacing user work with empty state; ignore
//! unrelated runtime fields. Empty bytes represent a new installation.
//! Core coordinates ordered saves and lifecycle flushes; native adapters own
//! private atomic writes. Restoration marks unconfirmed sends as delivery-unknown
//! until Host evidence reconciles them, preserving their content and order
//! without automatically resending.
use crate::state::{Draft, FileDraft, Navigation, PendingSubmission, Same, ScopedData, Snapshot};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// One device's editor selection, shared across Host snapshots. Discovery is never persisted.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EditorPreferences {
    pub preferred: Option<agent_protocol::editors::EditorId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ModelPreferences;
    use agent_protocol::editors::EditorId;

    #[test]
    fn welcome_and_folderless_new_chat_restore_independent_user_work() {
        use crate::state::{Attachment, DraftKey};
        use agent_protocol::composer::{Invocation, InvocationKind};
        let mut snapshot = Snapshot::default();
        let welcome = DraftKey::Local { key: String::new() };
        let new = DraftKey::Local { key: "new:".into() };
        let draft = |name: &str| {
            Arc::new(Draft {
                text: name.into(),
                attachments: vec![Attachment {
                    path: format!("/{name}.png"),
                    name: format!("{name}.png"),
                    is_image: true,
                }],
                invocations: vec![Invocation {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    kind: InvocationKind::Skill,
                    name: name.into(),
                    path: format!("/{name}/SKILL.md"),
                }],
                ..Default::default()
            })
        };
        let first = draft("welcome");
        let second = draft("new-chat");
        Arc::make_mut(&mut snapshot.input.drafts).insert(welcome.clone(), first.clone());
        Arc::make_mut(&mut snapshot.input.drafts).insert(new, second.clone());
        let restored = decode(&encode(&snapshot).unwrap()).unwrap();
        assert_eq!(restored.input.drafts.len(), 2);
        assert_eq!(restored.input.drafts[&welcome], first);
        let id = &restored.input.new_drafts["0:"];
        assert_eq!(restored.input.drafts[&DraftKey::from(id)], second);
        assert_eq!(
            restored.panes.primary().navigation,
            snapshot.panes.primary().navigation
        );
        let again = decode(&encode(&restored).unwrap()).unwrap();
        assert_eq!(again.input.drafts, restored.input.drafts);
        assert_eq!(again.input.new_drafts, restored.input.new_drafts);
    }

    #[test]
    fn host_state_excludes_device_model_preferences() {
        let mut snapshot = Snapshot::default();
        snapshot.catalog.model_preferences.provider =
            Some(agent_protocol::session::ProviderKind::Claude);
        let bytes = encode(&snapshot).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(saved.get("model_preferences").is_none());
        assert_eq!(
            decode(&bytes).unwrap().catalog.model_preferences,
            ModelPreferences::default()
        );
    }

    #[test]
    fn editor_preferences_are_device_owned_and_never_serialize_host_discovery() {
        let snapshot = Snapshot {
            files: crate::state::Files {
                preferred_editor: Some(EditorId::Cursor),
                file_open_capabilities: Some(Arc::new(
                    agent_protocol::editors::FileOpenCapabilities {
                        editors: vec![EditorId::Cursor, EditorId::FileManager],
                        reveal: None,
                    },
                )),
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&EditorPreferences {
            preferred: snapshot.files.preferred_editor,
        })
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"preferred":"cursor"})
        );
        assert_eq!(
            serde_json::from_slice::<EditorPreferences>(&bytes)
                .unwrap()
                .preferred,
            Some(EditorId::Cursor)
        );
        assert!(
            decode(&encode(&snapshot).unwrap())
                .unwrap()
                .files
                .file_open_capabilities
                .is_none()
        );
    }
}

/// Required user-owned fields prevent a damaged document from silently becoming
/// an empty draft. Runtime Snapshot fields cannot change the storage contract.
#[derive(Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    input_recoveries:
        Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<crate::state::InputRecovery>>>,
    #[serde(default)]
    connection_disabled: bool,
    #[serde(default)]
    project_preferences: Arc<BTreeMap<String, BTreeMap<String, crate::state::ProjectPreferences>>>,
    storage_scope: String,
    archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
    #[serde(with = "entries")]
    drafts: Arc<BTreeMap<crate::state::DraftKey, Arc<Draft>>>,
    #[serde(default)]
    new_drafts: Arc<BTreeMap<String, crate::session::SessionRef>>,
    pending_submissions: Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
    file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    navigation: Arc<Navigation>,
}

impl Same for PersistedState {
    fn same(&self, other: &Self) -> bool {
        self.input_recoveries.same(&other.input_recoveries)
            && self.connection_disabled == other.connection_disabled
            && self.project_preferences.same(&other.project_preferences)
            && self.storage_scope == other.storage_scope
            && self.archived_scopes.same(&other.archived_scopes)
            && self.drafts.same(&other.drafts)
            && self.new_drafts.same(&other.new_drafts)
            && self.pending_submissions.same(&other.pending_submissions)
            && self.file_drafts.same(&other.file_drafts)
            && self.navigation.same(&other.navigation)
    }
}

impl PersistedState {
    pub fn capture(snapshot: &Snapshot) -> Self {
        Self {
            input_recoveries: snapshot.input.input_recoveries.clone(),
            connection_disabled: snapshot.connection.connection_disabled,
            project_preferences: snapshot.lists.project_preferences.clone(),
            storage_scope: snapshot.connection.storage_scope.clone(),
            archived_scopes: snapshot.lists.archived_scopes.clone(),
            drafts: snapshot.input.drafts.clone(),
            new_drafts: snapshot.input.new_drafts.clone(),
            pending_submissions: snapshot.sessions.pending_submissions.clone(),
            file_drafts: snapshot.files.file_drafts.clone(),
            navigation: snapshot.panes.primary().navigation.clone(),
        }
    }
}

pub fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&PersistedState::capture(snapshot))
}

pub fn decode(bytes: &[u8]) -> Result<Snapshot, serde_json::Error> {
    if bytes.is_empty() {
        return Ok(Snapshot::default());
    }
    let mut saved: PersistedState = serde_json::from_slice(bytes)?;
    assign_draft_addresses(
        &mut saved.drafts,
        &mut saved.new_drafts,
        &mut saved.navigation,
        &mut saved.pending_submissions,
    );
    for scope in Arc::make_mut(&mut saved.archived_scopes).values_mut() {
        let scope = Arc::make_mut(scope);
        assign_draft_addresses(
            &mut scope.drafts,
            &mut scope.new_drafts,
            &mut scope.navigation,
            &mut scope.pending_submissions,
        );
    }
    Ok(Snapshot {
        files: crate::state::Files {
            file_drafts: saved.file_drafts,
            ..Default::default()
        },
        lists: crate::state::Lists {
            project_preferences: saved.project_preferences,
            archived_scopes: saved.archived_scopes,
            ..Default::default()
        },
        connection: crate::state::Connection {
            connection_disabled: saved.connection_disabled,
            storage_scope: saved.storage_scope,
            ..Default::default()
        },
        input: crate::state::Input {
            input_recoveries: saved.input_recoveries,
            drafts: saved.drafts,
            new_drafts: saved.new_drafts,
            ..Default::default()
        },
        sessions: crate::state::Sessions {
            pending_submissions: saved.pending_submissions,
            ..Default::default()
        },
        panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(
            crate::state::PaneState {
                navigation: saved.navigation,
                ..Default::default()
            },
        )),
        ..Default::default()
    })
}

// Pre-cutover drafts have no Host address yet. Allocate it once on restore,
// including archived Host scopes, before any view or pending send sees them.
fn assign_draft_addresses(
    drafts: &mut Arc<BTreeMap<crate::state::DraftKey, Arc<Draft>>>,
    new_drafts: &mut Arc<BTreeMap<String, crate::session::SessionRef>>,
    navigation: &mut Arc<Navigation>,
    pending: &mut Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
) {
    use crate::state::DraftKey;
    let keys: Vec<_> = drafts
        .keys()
        .chain(std::iter::once(&navigation.draft_key))
        .chain(pending.values().map(|input| &input.draft_key))
        .filter_map(|key| match key {
            DraftKey::Local { key: value } => value
                .strip_prefix("new:")
                .map(|suffix| (key.clone(), suffix.to_owned())),
            _ => None,
        })
        .collect();
    for (old, suffix) in keys {
        let project = suffix
            .split_once(':')
            .filter(|(slot, _)| slot.parse::<u64>().is_ok())
            .map_or_else(
                || format!("0:{suffix}"),
                |(slot, cwd)| format!("{slot}:{cwd}"),
            );
        let session = Arc::make_mut(new_drafts)
            .entry(project.clone())
            .or_insert_with(|| crate::session::SessionRef {
                provider: None,
                id: uuid::Uuid::new_v4().to_string(),
            })
            .clone();
        let key = DraftKey::from(session);
        if let Some(draft) = Arc::make_mut(drafts).remove(&old) {
            Arc::make_mut(drafts).insert(key.clone(), draft);
        }
        if navigation.draft_key == old {
            Arc::make_mut(navigation).draft_key = key.clone();
        }
        for input in Arc::make_mut(pending)
            .values_mut()
            .filter(|input| input.draft_key == old)
        {
            let input = Arc::make_mut(input);
            input.draft_key = key.clone();
            input.new_draft_slot = Some(project.clone());
        }
    }
    Arc::make_mut(new_drafts).retain(|_, id| {
        !pending
            .values()
            .any(|input| input.draft_key == DraftKey::from(id.clone()))
    });
}

/// Structured domain keys are stored as entries, without encoding keys into strings.
pub(crate) mod entries {
    use serde::{Deserialize, Serialize};
    use std::{collections::BTreeMap, sync::Arc};
    pub fn serialize<S: serde::Serializer, K: Serialize, V: Serialize>(
        values: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(values.iter())
    }
    pub fn deserialize<
        'de,
        D: serde::Deserializer<'de>,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    >(
        deserializer: D,
    ) -> Result<Arc<BTreeMap<K, V>>, D::Error> {
        let entries = Vec::<(K, V)>::deserialize(deserializer)?;
        let mut values = BTreeMap::new();
        for (key, value) in entries {
            if values.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate domain key"));
            }
        }
        Ok(Arc::new(values))
    }
}
