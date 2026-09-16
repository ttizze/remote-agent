use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListThreads {
    #[serde(flatten)]
    pub query: ListQuery,
}
impl ListThreads {
    pub fn new(query: ListQuery) -> Self {
        Self { query }
    }
}
rpc::rpc_method!(ListThreads, ThreadList, "host/thread/list");

impl Operation for ListThreads {
    rpc_operation!();
    fn invalidates(&self, snapshot: &Snapshot) -> bool {
        self.query != *snapshot.list_query
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
        snapshot.list_query = Arc::new(self.query.clone());
        Ok(())
    }
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, mut threads: Self::Output) -> Vec<Effect> {
        if let Some(errors) = threads
            .extra
            .get("providerErrors")
            .and_then(Value::as_object)
            && let Some(previous) = &snapshot.threads
        {
            for cached in &previous.data {
                let Some(id) = cached.id.as_ref() else {
                    continue;
                };
                let provider = if id.starts_with("claude:") {
                    "claude"
                } else {
                    "codex"
                };
                if errors.contains_key(provider)
                    && !threads
                        .data
                        .iter()
                        .any(|thread| thread.id.as_ref() == Some(id))
                {
                    let mut cached = cached.clone();
                    cached.extra.insert("listStale".into(), true.into());
                    cached.status = None;
                    threads.data.push(cached);
                }
            }
            for project in &previous.projects {
                if !threads
                    .projects
                    .iter()
                    .any(|current| current.id == project.id)
                    && threads.data.iter().any(|thread| {
                        thread.project_id.as_ref().and_then(Option::as_ref) == Some(&project.id)
                    })
                {
                    threads.projects.push(project.clone());
                }
            }
        }
        for summary in &threads.data {
            if let Some(id) = &summary.id
                && snapshot
                    .conversations
                    .get(id)
                    .is_some_and(|thread| thread.name != summary.name)
                && let Some(thread) = shared_mut(&mut snapshot.conversations, id)
            {
                thread.name = summary.name.clone();
            }
        }
        snapshot.threads = Some(Arc::new(threads));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}
impl rpc::RpcMethod for ReadItem {
    type Output = rpc::ItemResponse;
    const METHOD: &'static str = "host/thread/item/read";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.item.id == self.item_id.as_str() {
            Ok(())
        } else {
            Err("item ID does not match")
        }
    }
}

impl Operation for ReadItem {
    rpc_operation!();
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let Self {
            thread_id, turn_id, ..
        } = self;
        let rpc::ItemResponse { item, .. } = output;
        *snapshot = upsert_item(snapshot, &thread_id, &turn_id, item);
        reconcile_pending(snapshot, &thread_id);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadThread {
    pub thread_id: String,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub open: bool,
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = 5))]
    pub limit: u32,
}
impl ReadThread {
    pub fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            open: false,
            limit: 5,
        }
    }
    pub fn open(thread_id: String) -> Self {
        Self {
            open: true,
            ..Self::new(thread_id)
        }
    }
}
impl rpc::RpcMethod for ReadThread {
    type Output = crate::session::OpenedSession;
    const METHOD: &'static str = "host/session/open";
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::session::OpenSession {
            session: crate::session::SessionRef::from_thread_id(&self.thread_id)
                .map_err(serde::ser::Error::custom)?,
            limit: self.limit as usize,
        }
        .serialize(serializer)
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.session.thread_id() != self.thread_id {
            return Err("session identity does not match");
        }
        rpc::validate_thread(&output.response, Some(&self.thread_id))
    }
}

impl Operation for ReadThread {
    rpc_operation!();
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        self.open
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        self.limit = self
            .limit
            .max(
                snapshot
                    .conversations
                    .get(&self.thread_id)
                    .map_or(5, |thread| {
                        let requested = thread
                            .extra
                            .get("historyLimit")
                            .and_then(Value::as_u64)
                            .unwrap_or(5)
                            .min(1000) as u32;
                        requested.max(thread.turns.as_ref().map_or(0, |turns| turns.len() as u32))
                    }),
            )
            .max(5);
        if self.open {
            let cwd = snapshot
                .conversations
                .get(&self.thread_id)
                .and_then(|thread| thread.cwd.as_ref())
                .or_else(|| {
                    snapshot
                        .threads
                        .as_ref()?
                        .data
                        .iter()
                        .find(|thread| thread.id.as_ref() == Some(&self.thread_id))?
                        .cwd
                        .as_ref()
                })
                .cloned()
                .unwrap_or_default();
            select_thread(snapshot, self.thread_id.clone(), cwd);
        }
        snapshot.error = None;
        Ok(())
    }
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, mut output: Self::Output) -> Vec<Effect> {
        if output
            .response
            .thread
            .extra
            .get("historyReadState")
            .is_some_and(|state| state["type"] == "unavailable")
            && let Some(cached) = snapshot.conversations.get(&self.thread_id)
        {
            output.response.thread.turns = cached.turns.clone();
        }
        let mut effects = if self.open {
            open_thread(snapshot, output.response.thread, output.response.model)
        } else {
            refresh_thread(snapshot, output.response.thread)
        };
        if let Some((old, _)) = Arc::make_mut(&mut snapshot.subscriptions)
            .insert(self.thread_id, (output.subscription_id, output.revision))
        {
            effects.push(Effect::execute(CloseSubscription {
                subscription_id: old.to_string(),
            }));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        // A late navigation result can warm the cache, but must never replace
        // an already subscribed, more recent view or change the selected draft.
        if !snapshot.subscriptions.contains_key(&self.thread_id) {
            refresh_thread(snapshot, output.response.thread);
        }
        vec![Effect::execute(CloseSubscription {
            subscription_id: output.subscription_id.to_string(),
        })]
    }
}

fn select_thread(snapshot: &mut Snapshot, id: String, cwd: String) {
    if snapshot.navigation.cwd != cwd {
        clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
    }
    let navigation = Arc::make_mut(&mut snapshot.navigation);
    navigation.thread_id = Some(id.clone());
    navigation.draft_key = id;
    navigation.cwd = cwd;
}

pub(super) fn open_thread(
    snapshot: &mut Snapshot,
    thread: Thread,
    model: Option<String>,
) -> Vec<Effect> {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    let mut effects = refresh_thread(snapshot, thread);
    if let Some(id) = id {
        effects.extend(navigate(
            snapshot,
            Navigation {
                thread_id: Some(id.clone()),
                draft_key: id.clone(),
                cwd,
            },
        ));
        if let Some(model) = model {
            let (updated, _) = reduce(
                snapshot,
                Event::Intent(Intent::SelectModel {
                    thread_id: id,
                    model,
                }),
            );
            *snapshot = updated;
        }
    }
    effects.extend(review_workspace(snapshot));
    effects
}

pub(super) fn refresh_thread(snapshot: &mut Snapshot, incoming: Thread) -> Vec<Effect> {
    let Some(id) = incoming.id.clone().filter(|id| !id.trim().is_empty()) else {
        snapshot.error = Some("thread ID is missing".into());
        return Vec::new();
    };
    let thread = incoming;
    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(thread));
    project_requests(snapshot);
    reconcile_pending(snapshot, &id);

    Vec::new()
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkThread {
    pub thread_id: String,
    pub last_turn_id: String,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub exclude_turns: bool,
}
impl ForkThread {
    pub fn new(thread_id: String, last_turn_id: String) -> Self {
        Self {
            thread_id,
            last_turn_id,
            exclude_turns: false,
        }
    }
}
impl rpc::RpcMethod for ForkThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "thread/fork";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, None)
    }
}

impl Operation for ForkThread {
    rpc_operation!();
    const INVALIDATES: bool = true;
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let id = output.thread.id.clone().expect("validated thread ID");
        let mut effects = open_thread(snapshot, output.thread, output.model);
        effects.push(Effect::execute(ReadThread::new(id)));
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListThreads::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.thread.id.clone().expect("validated thread ID"),
        }
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartThread {
    #[serde(skip_serializing_if = "empty_cwd")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
fn empty_cwd(cwd: &Option<String>) -> bool {
    cwd.as_deref().is_none_or(|cwd| cwd.trim().is_empty())
}
impl rpc::RpcMethod for StartThread {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/start";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, None)
    }
}

impl Operation for StartThread {
    rpc_operation!();
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        refresh_thread(snapshot, output.thread)
    }
    const APPLY_WHEN_STALE: bool = true;
    fn outcome(output: &mut Self::Output) -> Outcome {
        ForkThread::outcome(output)
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interrupt {
    pub thread_id: String,
    pub turn_id: String,
}
rpc::rpc_method!(Interrupt, Map<String, Value>, "turn/interrupt");

impl Operation for Interrupt {
    rpc_operation!();
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSubscription {
    pub subscription_id: String,
}
rpc::rpc_method!(CloseSubscription, Map<String, Value>, "host/session/close");
impl Operation for CloseSubscription {
    rpc_operation!();
    fn disconnected_is_complete(&self) -> bool {
        true
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModels {}
impl Operation for LoadModels {
    type Output = crate::client::ModelPage;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.models().await
    }
    fn apply(self, snapshot: &mut Snapshot, catalog: Self::Output) -> Vec<Effect> {
        let models = catalog.data;
        let errors = catalog
            .extra
            .get("providerErrors")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let drafts = snapshot.drafts.clone();
        for (id, previous_draft) in drafts.iter() {
            let settings = supported_settings(previous_draft, &models, &errors);
            if settings
                != (
                    previous_draft.model.as_deref(),
                    previous_draft.effort.as_deref(),
                    previous_draft.service_tier.as_deref(),
                )
                && let Some(draft) = shared_mut(&mut snapshot.drafts, id)
            {
                draft.model = settings.0.map(str::to_owned);
                draft.effort = settings.1.map(str::to_owned);
                draft.service_tier = settings.2.map(str::to_owned);
            }
        }
        snapshot.models = Arc::new(models);
        snapshot.model_errors = Arc::new(errors);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub request_id: Value,
}
impl rpc::RpcMethod for OpenRequest {
    type Output = crate::session::SessionRef;
    const METHOD: &'static str = "host/session/request";
}
impl Operation for OpenRequest {
    type Output = crate::session::OpenedSession;
    const ORDERED: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let session = context.client.call(self).await?.value;
        let mut open = ReadThread::new(session.thread_id());
        let mut snapshot = context.snapshot.clone();
        open.prepare(&mut snapshot)
            .map_err(PeerError::InvalidMessage)?;
        context.call(&open).await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        ReadThread::new(output.session.thread_id()).apply(snapshot, output)
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        ReadThread::new(output.session.thread_id()).stale(snapshot, output)
    }
}
