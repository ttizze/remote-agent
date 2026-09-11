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
impl rpc::RpcMethod for ListThreads {
    type Output = ThreadList;
    const METHOD: &'static str = "host/thread/list";
}

impl Operation for ListThreads {
    rpc_operation!();
    fn invalidates(&self, snapshot: &Snapshot) -> bool {
        self.query != *snapshot.list_query
    }
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.list_query = Arc::new(self.query.clone());
        Ok(())
    }
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, threads: Self::Output) -> Vec<Effect> {
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
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub include_turns: bool,
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub paginate_history: bool,
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub defer_item_details: bool,
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub open: bool,
}
impl ReadThread {
    pub fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            include_turns: true,
            paginate_history: true,
            defer_item_details: true,
            open: false,
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
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/read";
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut params = serializer.serialize_struct("ReadThread", 4)?;
        params.serialize_field("threadId", &self.thread_id)?;
        params.serialize_field("includeTurns", &self.include_turns)?;
        params.serialize_field("paginateHistory", &self.paginate_history)?;
        params.serialize_field("deferItemDetails", &self.defer_item_details)?;
        params.end()
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, Some(self.thread_id.as_str()))
    }
}

impl Operation for ReadThread {
    rpc_operation!();
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        self.open
    }
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if self.open {
            open_thread(snapshot, output.thread, output.model)
        } else {
            refresh_thread(snapshot, output.thread)
        }
    }
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if self.open {
            refresh_thread(snapshot, output.thread)
        } else {
            Vec::new()
        }
    }
}

pub(super) fn open_thread(
    snapshot: &mut Snapshot,
    thread: Thread,
    model: Option<String>,
) -> Vec<Effect> {
    let id = thread.id.clone();
    let cwd = thread.cwd.clone().unwrap_or_default();
    let path = thread.path.clone();
    let external = thread
        .status
        .as_ref()
        .is_some_and(|status| status.kind == "notLoaded");
    let mut effects = refresh_thread(snapshot, thread);
    if let Some(id) = id {
        if snapshot.navigation.cwd != cwd {
            clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
        }
        let navigation = Arc::make_mut(&mut snapshot.navigation);
        if let Some(watch_id) = navigation.watch_id.take() {
            effects.push(Effect::execute(Unwatch {
                watch_key: 1,
                watch_id,
            }));
        }
        navigation.watch_thread_id = None;
        navigation.thread_id = Some(id.clone());
        if snapshot.activity.unread.contains(&id) {
            Arc::make_mut(&mut snapshot.activity).unread.remove(&id);
        }
        navigation.draft_key = id.clone();
        navigation.cwd = cwd;
        // Loaded threads stream native events. Their advertised rollout may
        // not be materialized yet, so file changes must not trigger hydration.
        if external && path.is_some() {
            navigation.watch_id = Some(snapshot.epoch);
            navigation.watch_thread_id = Some(id.clone());
            effects.push(Effect::execute(Watch {
                thread_id: id.clone(),
                watch_key: 1,
                watch_id: snapshot.epoch,
                path,
            }));
        }
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
    let thread = match snapshot.conversations.get(&id) {
        Some(current) => refresh(current, &incoming),
        None => incoming,
    };
    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(thread));
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
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let mut effects = open_thread(snapshot, output.thread, output.model);
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
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.apply(snapshot, output)
    }
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
impl rpc::RpcMethod for Interrupt {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "turn/interrupt";
}

impl Operation for Interrupt {
    rpc_operation!();
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watch {
    pub thread_id: String,
    pub watch_key: u64,
    pub watch_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
impl rpc::RpcMethod for Watch {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/thread/watch";
}

impl Operation for Watch {
    rpc_operation!();
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        let navigation = Arc::make_mut(&mut snapshot.navigation);
        navigation.watch_id = Some(self.watch_id);
        navigation.watch_thread_id = Some(self.thread_id.clone());
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unwatch {
    pub watch_key: u64,
    pub watch_id: u64,
}
impl rpc::RpcMethod for Unwatch {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/thread/unwatch";
}

impl Operation for Unwatch {
    rpc_operation!();
    fn disconnected_is_complete(&self) -> bool {
        true
    }
    fn prepare(&self, snapshot: &mut Snapshot) -> Result<(), String> {
        if snapshot.navigation.watch_id == Some(self.watch_id) {
            let navigation = Arc::make_mut(&mut snapshot.navigation);
            navigation.watch_id = None;
            navigation.watch_thread_id = None;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadOlder {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub cursor: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = true))]
    pub defer_item_details: bool,
}
impl ReadOlder {
    pub fn new(thread_id: String, turn_id: Option<String>, cursor: Option<String>) -> Self {
        Self {
            thread_id,
            turn_id,
            cursor,
            defer_item_details: true,
        }
    }
}
impl rpc::RpcMethod for ReadOlder {
    type Output = crate::models::ThreadResponse;
    const METHOD: &'static str = "host/thread/turns/list";
    fn method(&self) -> &'static str {
        if self.turn_id.is_some() {
            "host/thread/items/list"
        } else {
            Self::METHOD
        }
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        rpc::validate_thread(output, Some(self.thread_id.as_str()))
    }
}

impl Operation for ReadOlder {
    rpc_operation!();
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let id = &self.thread_id;
        if let Some(current) = snapshot.conversations.get(id) {
            match older(
                current,
                &output.thread,
                self.turn_id.as_deref(),
                self.cursor.as_deref(),
            ) {
                Ok(merged) => {
                    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(merged));
                    reconcile_pending(snapshot, id);
                }
                Err(error) => snapshot.error = Some(error),
            }
        }

        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModels {}
impl Operation for LoadModels {
    type Output = Vec<Model>;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.models().await
    }
    fn apply(self, snapshot: &mut Snapshot, models: Self::Output) -> Vec<Effect> {
        let drafts = snapshot.drafts.clone();
        for (id, previous_draft) in drafts.iter() {
            let settings = supported_settings(previous_draft, &models);
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
        Vec::new()
    }
}
