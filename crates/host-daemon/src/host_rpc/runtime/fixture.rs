//! UI archives seed already observed execution, independently of text imports.
use super::*;
use agent_protocol::orchestration as model;

impl HostRpcService {
    pub async fn publish_fixture_history(&self, native: &SessionRef) -> Result<(), String> {
        let storage = &self.inner.conversation.storage;
        if storage.native_owner(native)?.is_some() {
            return Ok(());
        }
        let agent = self
            .inner
            .providers
            .discovery(native.native_provider().map_err(str::to_owned)?)
            .map_err(|error| error.message)?;
        let mut response = agent
            .open(&native.id, usize::MAX, true)
            .await
            .map_err(|error| error.message)?;
        let mut cursors = std::collections::HashSet::new();
        while let Some(cursor) = response.thread.history_cursor.take() {
            if !cursors.insert(cursor.clone()) {
                return Err("fixture history cursor repeated".into());
            }
            let page = agent
                .read_history(&native.id, &cursor, true)
                .await
                .map_err(|error| error.message)?;
            response.thread.turns = Some(agent_protocol::session::prepend_history(
                &page.turns,
                response.thread.turns.as_deref().unwrap_or_default(),
            ));
            response.thread.history_cursor = page.next_cursor;
        }
        let now = commands::at();
        let (mut thread, source) =
            Runtime::native_header_records(self, native, native, &response, &now)
                .await
                .map_err(|error| error.message)?;
        // These are a user's previously observed events; opening must not replace
        // them with a new static text import.
        thread.created_by = model::Actor::User;
        let records = archive(
            &thread,
            &source,
            native,
            response.thread.turns.unwrap_or_default(),
            &thread.updated_at,
        )?;
        storage.seed_native_records(&thread, &source, records)?;
        Ok(())
    }
}

fn archive(
    thread: &model::Thread,
    source: &model::ProviderThread,
    native: &SessionRef,
    turns: Vec<Arc<agent_protocol::models::Turn>>,
    now: &str,
) -> Result<Vec<model::Change>, String> {
    let mut state = model::State::new(thread.clone());
    state
        .provider_threads
        .insert(source.id.clone(), source.clone());
    let mut archive = Vec::new();
    for turn in turns {
        let reference = super::native::control_reference(
            native.native_provider().map_err(str::to_owned)?,
            turn.id.as_str(),
        );
        let started = model::observe_external_turn(
            &state,
            &source.id,
            &native.id,
            reference.clone(),
            model::ProviderTurnStatus::Running,
            now,
        )?;
        state = state.apply(&started);
        archive.extend(started.changes);
        let completed = turn.status != agent_protocol::execution::TurnStatus::Running;
        let context = native::external::context(&state, native, &turn.id, now, completed)
            .ok_or("fixture turn has no native context")?;
        let mut items = model::Decision::default();
        for item in turn.items.iter().flatten() {
            if item.is_deferred() {
                return Err("fixture archive requires complete native bodies".into());
            }
            for artifact in context.item(item)? {
                items.changes.push(model::Change::Node(artifact.node));
                items.changes.push(model::Change::TurnItem(artifact.item));
                if let Some(plan) = artifact.plan {
                    items.changes.push(model::Change::Plan(plan));
                }
            }
        }
        state = state.apply(&items);
        archive.extend(items.changes);
        if completed {
            let status = native::completed_turn_status(turn.status);
            let settled = model::observe_external_turn(
                &state, &source.id, &native.id, reference, status, now,
            )?;
            state = state.apply(&settled);
            archive.extend(settled.changes);
        }
    }
    Ok(archive)
}
