use super::*;

pub(super) struct Source {
    pub target: SessionRef,
    pub session_id: String,
    pub io: AsyncMutex<()>,
    pub(super) updates: AsyncMutex<()>,
    pub(super) text: Mutex<super::text::Text>,
    background: Mutex<HashMap<Uuid, Background>>,
    images: Mutex<HashMap<(Uuid, String), Vec<String>>>,
}

#[derive(Default)]
struct Background {
    tasks: Vec<agent_protocol::execution::BackgroundTask>,
    native_ended: bool,
}

#[derive(Clone)]
pub(super) struct PendingRequest {
    pub target: SessionRef,
    pub native: SessionRef,
    pub attempt_id: Option<Uuid>,
    pub origin: RequestOrigin,
    pub body: agent_protocol::requests::RequestBody,
}

impl Source {
    pub(super) fn record_generated_image(&self, attempt: Uuid, turn: &str, item: &str) {
        let mut pending = self.images.lock().unwrap();
        let images = pending.entry((attempt, turn.into())).or_default();
        if !images.iter().any(|saved| saved == item) {
            images.push(item.into());
        }
    }
    pub(super) fn generated_images(&self, attempt: Uuid, turn: &str) -> Vec<String> {
        self.images
            .lock()
            .unwrap()
            .get(&(attempt, turn.into()))
            .cloned()
            .unwrap_or_default()
    }
    pub(super) fn clear_generated_images(&self, attempt: Uuid, turn: &str) {
        self.images.lock().unwrap().remove(&(attempt, turn.into()));
    }

    pub(super) fn cancel_background(&self, execution: Uuid) {
        self.background.lock().unwrap().remove(&execution);
    }

    pub(super) fn background_owners(
        &self,
    ) -> Vec<(Uuid, Vec<agent_protocol::execution::BackgroundTask>)> {
        self.background
            .lock()
            .unwrap()
            .iter()
            .map(|(owner, background)| (*owner, background.tasks.clone()))
            .collect()
    }

    pub(super) fn finish_foreground(
        &self,
        execution: Uuid,
        fallback: Vec<agent_protocol::execution::BackgroundTask>,
    ) -> Vec<agent_protocol::execution::BackgroundTask> {
        let mut roster = self.background.lock().unwrap();
        if let Some(background) = roster.get_mut(&execution) {
            background.native_ended = true;
            return background.tasks.clone();
        }
        if !fallback.is_empty() {
            roster.insert(
                execution,
                Background {
                    tasks: fallback.clone(),
                    native_ended: true,
                },
            );
        }
        fallback
    }

    pub(super) fn roster(
        &self,
        execution: Uuid,
        tasks: Vec<agent_protocol::execution::BackgroundTask>,
    ) -> Vec<(Uuid, Vec<agent_protocol::execution::BackgroundTask>, bool)> {
        let mut roster = self.background.lock().unwrap();
        let previous = std::mem::take(&mut *roster);
        for task in tasks {
            let owner = previous
                .iter()
                .find_map(|(owner, background)| {
                    background
                        .tasks
                        .iter()
                        .any(|known| known.id == task.id)
                        .then_some(*owner)
                })
                .unwrap_or(execution);
            let background = roster.entry(owner).or_insert_with(|| Background {
                native_ended: previous
                    .get(&owner)
                    .is_some_and(|background| background.native_ended),
                ..Default::default()
            });
            background.tasks.push(task);
        }
        let executions: std::collections::HashSet<_> =
            previous.keys().chain(roster.keys()).copied().collect();
        executions
            .into_iter()
            .map(|execution| {
                (
                    execution,
                    roster
                        .get(&execution)
                        .map(|background| background.tasks.clone())
                        .unwrap_or_default(),
                    previous
                        .get(&execution)
                        .is_some_and(|background| background.native_ended),
                )
            })
            .collect()
    }
}

impl Runtime {
    pub(super) fn retain_request(
        &self,
        id: RequestId,
        bytes: usize,
        request: PendingRequest,
    ) -> Result<bool, String> {
        if bytes > 64 * 1024 {
            return Err("pending request capacity reached".into());
        }
        let mut requests = self.requests.lock().unwrap();
        if requests.contains_key(&id) {
            return Ok(false);
        }
        if requests.len() >= 256
            || requests
                .values()
                .filter(|pending| pending.target == request.target)
                .count()
                >= 32
        {
            return Err("pending request capacity reached".into());
        }
        requests.insert(id, request);
        Ok(true)
    }

    pub(super) fn prune_requests(&self, target: &SessionRef) {
        let retained: Vec<_> = self
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, request)| request.target.application_id() == target.application_id())
            .map(|(id, request)| (id.clone(), request.origin.source.clone()))
            .collect();
        for (id, source) in retained {
            match self.storage.request_owner(&id) {
                Ok(owner)
                    if owner.as_ref().map(SessionRef::application_id)
                        != Some(target.application_id()) =>
                {
                    let mut requests = self.requests.lock().unwrap();
                    if requests
                        .get(&id)
                        .is_some_and(|request| Arc::ptr_eq(&request.origin.source, &source))
                    {
                        requests.remove(&id);
                    }
                }
                Err(error) => {
                    tracing::error!(target:"agent",operation="conversation.requests.release",%error)
                }
                _ => {}
            }
        }
    }

    pub(super) fn register_native(&self, target: &SessionRef, native: &SessionRef) -> Arc<Source> {
        self.resources
            .lock()
            .unwrap()
            .entry(native.clone())
            .or_insert_with(|| {
                Arc::new(Source {
                    target: target.clone(),
                    session_id: agent_protocol::orchestration::derived_id(
                        "provider-session",
                        &[
                            "provider-instance",
                            native
                                .native_provider()
                                .expect("native resource has a provider")
                                .key(),
                            "shared",
                        ],
                    ),
                    io: Default::default(),
                    updates: Default::default(),
                    text: Default::default(),
                    background: Default::default(),
                    images: Default::default(),
                })
            })
            .clone()
    }

    /// The registry lock and the last I/O holder serialize retirement with the
    /// next lookup. Correlation remains in storage for late native callbacks.
    pub(super) fn retire_source(&self, native: &SessionRef) {
        let mut sources = self.resources.lock().unwrap();
        if sources.get(native).is_some_and(|source| {
            Arc::strong_count(source) == 1
                && source.background.lock().unwrap().is_empty()
                && source.images.lock().unwrap().is_empty()
                && source.text.lock().unwrap().is_empty()
        }) {
            sources.remove(native);
        }
    }
}

#[cfg(test)]
mod tests;
