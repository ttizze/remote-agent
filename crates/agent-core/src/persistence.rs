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
pub(crate) mod tests {
    use super::*;
    use crate::state::ModelPreferences;
    use agent_protocol::editors::EditorId;

    pub(crate) fn lean_cases<T: serde::de::DeserializeOwned>(kind: &str, count: usize) -> Vec<T> {
        let dir = std::env::var_os("AGENT_CORE_CASES").expect("set AGENT_CORE_CASES");
        let path = std::path::PathBuf::from(dir).join("persistence.jsonl");
        let input = std::fs::read_to_string(path).expect("read Lean persistence fixtures");
        let cases: Vec<_> = input
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|row| row["kind"] == kind)
            .map(|row| serde_json::from_value(row).unwrap())
            .collect();
        assert_eq!(cases.len(), count, "incomplete {kind} corpus");
        cases
    }

    #[test]
    #[ignore = "requires Lean-generated persistence.jsonl"]
    fn lean_core_persistence_conformance() {
        use crate::state::DraftKey;
        use serde_json::{Value, json};

        #[derive(Debug, Deserialize)]
        struct Case {
            keys: Vec<String>,
            accepted: bool,
        }

        for case in lean_cases::<Case>("drafts", 6) {
            let bytes = encode(&Snapshot::default()).unwrap();
            let mut saved: Value = serde_json::from_slice(&bytes).unwrap();
            saved["drafts"] = Value::Array(
                case.keys
                    .iter()
                    .enumerate()
                    .map(|(index, key)| {
                        let draft = Draft {
                            text: index.to_string(),
                            ..Default::default()
                        };
                        json!([DraftKey::from(key.as_str()), draft])
                    })
                    .collect(),
            );
            let restored = decode(&serde_json::to_vec(&saved).unwrap());
            assert_eq!(restored.is_ok(), case.accepted, "{case:?}");
            if let Ok(restored) = restored {
                assert_eq!(restored.input.drafts.len(), case.keys.len());
                for (index, key) in case.keys.iter().enumerate() {
                    let draft = &restored.input.drafts[&DraftKey::from(key.as_str())];
                    assert_eq!(draft.text, index.to_string());
                }
            }
        }
    }

    #[test]
    fn welcome_and_folderless_new_chat_restore_independent_user_work() {
        use crate::state::{Attachment, DraftKey};
        use agent_protocol::composer::{Invocation, InvocationKind};
        let mut snapshot = Snapshot::default();
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
        use crate::state::{Event, Intent, reduce};
        let apply = |snapshot: &Snapshot, intent| reduce(snapshot, "", Event::from(intent)).0;
        snapshot = apply(
            &snapshot,
            Intent::SetDraft {
                thread_id: DraftKey::default(),
                draft: (*first).clone(),
            },
        );
        snapshot = apply(&snapshot, Intent::NewChat { cwd: String::new() });
        let new = snapshot.panes.primary().navigation.draft_key.clone();
        snapshot = apply(
            &snapshot,
            Intent::SetDraft {
                thread_id: new,
                draft: (*second).clone(),
            },
        );
        snapshot = apply(&snapshot, Intent::ShowThreadList);
        let restored = decode(&encode(&snapshot).unwrap()).unwrap();
        assert_eq!(restored.input.drafts.len(), 2);
        let welcome = DraftKey::from(&restored.input.new_drafts[crate::state::WELCOME_DRAFT_SLOT]);
        assert_eq!(restored.input.drafts[&welcome], first);
        let id = &restored.input.new_drafts["0:"];
        assert_eq!(restored.input.drafts[&DraftKey::from(id)], second);
        assert_eq!(restored.panes.primary().navigation.draft_key, welcome);
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
    #[serde(default, with = "entries", skip_serializing_if = "BTreeMap::is_empty")]
    queued_edits: Arc<BTreeMap<crate::session::SessionRef, crate::state::follow_up::QueuedEdit>>,
    #[serde(default)]
    stashes: Arc<Vec<crate::composer::stash::StashedPrompt>>,
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
            && self.queued_edits.same(&other.queued_edits)
            && self.stashes.same(&other.stashes)
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
            queued_edits: snapshot.input.queued_edits.clone(),
            stashes: snapshot.input.stashes.clone(),
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
    let saved: PersistedState = serde_json::from_slice(bytes)?;
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
            queued_edits: saved.queued_edits,
            stashes: saved.stashes,
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

/// Structured domain keys are stored as entries, without encoding keys into strings.
pub(crate) mod entries {
    use serde::{Deserialize, Serialize};
    use std::{collections::BTreeMap, sync::Arc};
    pub(crate) fn serialize<S: serde::Serializer, K: Serialize, V: Serialize>(
        values: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(values.iter())
    }
    pub(crate) fn deserialize<
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
