//! Native reads do not expose every selected option (notably Codex speed).
//! Retain accepted turn settings on the Host so every client can reopen them.
use crate::host_rpc::storage::Database;
use crate::providers::Failure;
use agent_protocol::{models::ModelRef, session::SessionRef};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Serialize, Deserialize)]
pub(in crate::host_rpc) struct Selection {
    pub session: SessionRef,
    #[serde(flatten)]
    pub options: SelectedOptions,
}

#[derive(PartialEq, Serialize, Deserialize)]
pub(in crate::host_rpc) struct SelectedOptions {
    pub model: Option<ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}

pub(super) struct ModelSelections {
    database: Arc<Database>,
    state: Mutex<Result<Selections, String>>,
}

struct Selections {
    entries: Vec<Selection>,
    dirty: bool,
}

impl ModelSelections {
    pub fn new(database: Arc<Database>) -> Self {
        let selections = database.model_selections();
        Self {
            database,
            state: Mutex::new(selections.map(|entries| Selections {
                entries,
                dirty: false,
            })),
        }
    }

    pub fn resolve(
        &self,
        session: &SessionRef,
        model: Option<&ModelRef>,
        effort: Option<&str>,
        service_tier: Option<&str>,
    ) -> Result<SelectedOptions, Failure> {
        let state = self.state.lock().unwrap();
        let state = state
            .as_ref()
            .map_err(|error| Failure::new("model_selection_storage_failed", error))?;
        // An externally changed native model takes precedence over old options.
        let saved = state
            .entries
            .iter()
            .find(|saved| &saved.session == session)
            .filter(|saved| model.is_none() || model == saved.options.model.as_ref())
            .map(|saved| &saved.options);
        Ok(SelectedOptions {
            model: model
                .or_else(|| saved.and_then(|saved| saved.model.as_ref()))
                .cloned(),
            effort: effort
                .or_else(|| saved.and_then(|saved| saved.effort.as_deref()))
                .map(str::to_owned),
            service_tier: service_tier
                .or_else(|| saved.and_then(|saved| saved.service_tier.as_deref()))
                .map(str::to_owned),
        })
    }

    pub fn record(
        &self,
        session: &SessionRef,
        model: Option<&ModelRef>,
        effort: Option<&str>,
        service_tier: Option<&str>,
    ) {
        let mut state = self.state.lock().unwrap();
        let Ok(state) = state.as_mut() else {
            return;
        };
        let next = Selection {
            session: session.clone(),
            options: SelectedOptions {
                model: model.cloned(),
                effort: effort.map(str::to_owned),
                service_tier: service_tier.map(str::to_owned),
            },
        };
        // Apply new live options even when an earlier save is still pending.
        state.dirty |= match state
            .entries
            .iter_mut()
            .find(|saved| &saved.session == session)
        {
            Some(saved) if saved.options == next.options => false,
            Some(saved) => {
                *saved = next;
                true
            }
            None => {
                state.entries.push(next);
                true
            }
        };
        if !state.dirty {
            return;
        }
        // Submission is already accepted. Keep the live fact and never invite a
        // duplicate send by reporting a storage failure as a submission failure.
        match self.database.save_model_selections(&state.entries) {
            Ok(()) => state.dirty = false,
            Err(error) => {
                tracing::warn!(target: "agent", operation = "host.model_selection.record", message = %error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::session::ProviderKind;
    fn load_selections(path: impl AsRef<std::path::Path>) -> ModelSelections {
        ModelSelections::new(Arc::new(Database::open(path.as_ref())))
    }

    #[test]
    fn persisted_options_fill_native_gaps_without_overriding_external_changes() {
        for native_model in [None, Some("saved"), Some("external")] {
            for native_effort in [None, Some("low")] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("models.sqlite");
                let session = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
                let model = |id: &str| ModelRef {
                    provider: session.provider,
                    id: id.into(),
                };
                load_selections(path.clone()).record(
                    &session,
                    Some(&model("saved")),
                    Some("high"),
                    Some("priority"),
                );
                let store = load_selections(path);
                let resolved = store
                    .resolve(
                        &session,
                        native_model.map(model).as_ref(),
                        native_effort,
                        None,
                    )
                    .unwrap();
                assert_eq!(resolved.model.unwrap().id, native_model.unwrap_or("saved"));
                let saved_applies = native_model != Some("external");
                assert_eq!(
                    resolved.effort.as_deref(),
                    native_effort.or(saved_applies.then_some("high"))
                );
                assert_eq!(
                    resolved.service_tier.as_deref(),
                    saved_applies.then_some("priority")
                );
                let other = SessionRef::new(ProviderKind::Claude, session.id).unwrap();
                let resolved = store.resolve(&other, None, None, None).unwrap();
                assert!(resolved.model.is_none());
                assert!(resolved.effort.is_none());
                assert!(resolved.service_tier.is_none());
            }
        }
    }

    #[test]
    fn failed_writes_keep_accepted_settings_live_and_later_writes_recover() {
        for (effort, tier) in [("high", "priority"), ("low", "default")] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("models.sqlite");
            let store = load_selections(path.clone());
            let session = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
            let model = ModelRef {
                provider: session.provider,
                id: "saved".into(),
            };
            store.database.with(|connection| connection.execute_batch("CREATE TRIGGER reject_selection BEFORE INSERT ON model_selections BEGIN SELECT RAISE(ABORT, 'test failure'); END;").map_err(crate::host_rpc::storage::failure)).unwrap();
            store.record(&session, Some(&model), Some("high"), Some("priority"));
            assert_eq!(
                store
                    .resolve(&session, None, None, None)
                    .unwrap()
                    .effort
                    .as_deref(),
                Some("high")
            );
            store
                .database
                .with(|connection| {
                    connection
                        .execute_batch("DROP TRIGGER reject_selection;")
                        .map_err(crate::host_rpc::storage::failure)
                })
                .unwrap();
            store.record(&session, Some(&model), Some(effort), Some(tier));
            let recovered = load_selections(path.clone())
                .resolve(&session, None, None, None)
                .unwrap();
            assert_eq!(recovered.effort.as_deref(), Some(effort));
            assert_eq!(recovered.service_tier.as_deref(), Some(tier));
            store.record(&session, Some(&model), Some("low"), Some("default"));
            let resolved = load_selections(path.clone())
                .resolve(&session, None, None, None)
                .unwrap();
            assert_eq!(resolved.effort.as_deref(), Some("low"));
            assert_eq!(resolved.service_tier.as_deref(), Some("default"));
            drop(store);
            std::fs::write(&path, "broken").unwrap();
            assert!(
                load_selections(path)
                    .resolve(&session, None, None, None)
                    .is_err()
            );
        }
    }

    #[test]
    fn unchanged_accepted_settings_do_not_rewrite_storage_including_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.sqlite");
        let store = load_selections(path.clone());
        let session = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        let model = ModelRef {
            provider: session.provider,
            id: "saved".into(),
        };
        store.record(&session, Some(&model), Some("high"), Some("priority"));
        let reopened = load_selections(path.clone());
        let observer = rusqlite::Connection::open(&path).unwrap();
        let version: i64 = observer
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap();
        for store in [&store, &reopened] {
            store.record(&session, Some(&model), Some("high"), Some("priority"));
            assert_eq!(
                observer
                    .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                version
            );
        }
    }
}
