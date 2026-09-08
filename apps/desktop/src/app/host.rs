use super::{Event, text};
use crate::{
    platform,
    rpc::{self, Rpc},
};
use gpui_kit::{App, AppContext, Entity, EntityId, EventEmitter};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    rc::{Rc, Weak},
    time::Duration,
};

#[derive(Default)]
pub(super) struct Management {
    pub(super) connected: bool,
    pub(super) status: Value,
    pub(super) hosts: Vec<Value>,
    attempted_start: bool,
}
pub(super) enum ManagementInput {
    Connected(bool),
    Status(Result<Value, String>),
    Hosts(Result<Value, String>),
    Started(Result<(), String>),
    Refresh,
}
pub(super) enum ManagementEffect {
    Refresh,
    StartHost,
    Error(String),
}

pub(super) fn reduce_management(
    mut state: Management,
    input: ManagementInput,
) -> (Management, Option<ManagementEffect>) {
    let effect = match input {
        ManagementInput::Connected(connected) => {
            state.connected = connected;
            if connected {
                Some(ManagementEffect::Refresh)
            } else if !state.attempted_start {
                state.attempted_start = true;
                Some(ManagementEffect::StartHost)
            } else {
                None
            }
        }
        ManagementInput::Status(Ok(value)) => {
            state.status = value;
            None
        }
        ManagementInput::Hosts(Ok(Value::Array(hosts))) => {
            state.hosts = hosts;
            None
        }
        ManagementInput::Hosts(Ok(_)) => {
            state.hosts.clear();
            None
        }
        ManagementInput::Started(Ok(())) => None,
        ManagementInput::Status(Err(error))
        | ManagementInput::Hosts(Err(error))
        | ManagementInput::Started(Err(error)) => Some(ManagementEffect::Error(error)),
        ManagementInput::Refresh => state.connected.then_some(ManagementEffect::Refresh),
    };
    (state, effect)
}

pub(super) fn connect_management(
    tx: &async_channel::Sender<Event>,
) -> (Rpc, Option<tokio::task::AbortHandle>) {
    let events = tx.clone();
    let rpc = Rpc::connect(
        platform::state_dir().join("host.sock"),
        json!({"target":"manager"}),
        move |event| {
            if let rpc::Event::Connected(connected, _) = event {
                let _ =
                    events.send_blocking(Event::Management(ManagementInput::Connected(connected)));
            }
        },
    );
    let clock = tx.clone();
    let timer = host_protocol::rpc_runtime().ok().map(|runtime| {
        runtime
            .spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    if clock
                        .send(Event::Management(ManagementInput::Refresh))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .abort_handle()
    });
    (rpc, timer)
}

pub(super) fn run_management(
    effect: ManagementEffect,
    rpc: &Rpc,
    tx: &async_channel::Sender<Event>,
) -> Option<String> {
    match effect {
        ManagementEffect::Refresh => {
            let events = tx.clone();
            rpc.request_async("host/status", json!({}), move |result| {
                let _ = events.send_blocking(Event::Management(ManagementInput::Status(result)));
            });
            let events = tx.clone();
            rpc.request_async("host/listRemotes", json!({}), move |result| {
                let _ = events.send_blocking(Event::Management(ManagementInput::Hosts(result)));
            });
        }
        ManagementEffect::StartHost => {
            let events = tx.clone();
            std::thread::spawn(move || {
                let _ = events.send_blocking(Event::Management(ManagementInput::Started(
                    platform::start_host(None),
                )));
            });
        }
        ManagementEffect::Error(error) => return Some(error),
    }
    None
}

/// A value projected by native views; it contains no connection or worker.
#[derive(Default)]
pub(super) struct Catalogue {
    pub(super) connected: bool,
    pub(super) projects: Vec<Value>,
    pub(super) threads: Vec<Value>,
    pub(super) models: Vec<Value>,
    pub(super) models_revision: u64,
    pub(super) query: Value,
    pub(super) more: Value,
    pub(super) indicators: TaskIndicators,
    pub(super) error: String,
    visible: Option<String>,
    list_generation: u64,
    model_generation: u64,
}
pub(super) enum Notice {
    Connected(bool),
    Error(String),
}
impl EventEmitter<Notice> for Catalogue {}
pub(super) enum CatalogueInput {
    Rpc(rpc::Event),
    Titles(u64, Result<Value, String>),
    Models(u64, Result<Vec<Value>, String>),
    Refresh,
    Search(String),
    MoreProjects,
    MoreChats,
    MoreProjectThreads(String),
    Visible(Option<String>),
}
#[derive(Default)]
pub(super) struct CatalogueEffects {
    titles: bool,
    models: bool,
    notice: Option<Notice>,
}

pub(super) fn reduce_catalogue(
    mut state: Catalogue,
    input: CatalogueInput,
) -> (Catalogue, CatalogueEffects) {
    let mut effects = CatalogueEffects::default();
    match input {
        CatalogueInput::Rpc(rpc::Event::Connected(connected, reason)) => {
            state.connected = connected;
            effects.notice = Some(Notice::Connected(connected));
            state.list_generation += 1;
            state.model_generation += 1;
            if connected {
                state.indicators.active.clear();
                state.error.clear();
                effects.titles = true;
                effects.models = true;
            } else {
                state.error = reason;
            }
        }
        CatalogueInput::Rpc(rpc::Event::Message(message)) => {
            state.indicators =
                reduce_indicators(state.indicators, &message, state.visible.as_deref());
        }
        CatalogueInput::Titles(generation, result) if generation == state.list_generation => {
            match result {
                Ok(mut value) => {
                    state.projects = match value["projects"].take() {
                        Value::Array(v) => v,
                        _ => Vec::new(),
                    };
                    state.threads = match value["data"].take() {
                        Value::Array(v) => v,
                        _ => Vec::new(),
                    };
                    state.more = value;
                }
                Err(error) => {
                    state.error = error.clone();
                    effects.notice = Some(Notice::Error(error));
                }
            }
        }
        CatalogueInput::Models(generation, result) if generation == state.model_generation => {
            match result {
                Ok(models) => {
                    state.models = models;
                    state.models_revision += 1;
                }
                Err(error) => {
                    state.error = error.clone();
                    effects.notice = Some(Notice::Error(error));
                }
            }
        }
        CatalogueInput::Refresh => effects.titles = state.connected,
        CatalogueInput::Search(search) => {
            if text(&state.query, "searchTerm") != search {
                state.query["searchTerm"] = search.into();
                effects.titles = state.connected;
            }
        }
        CatalogueInput::MoreProjects => {
            state.query["projectLimit"] =
                json!(state.query["projectLimit"].as_u64().unwrap_or(10) + 10);
            effects.titles = state.connected;
        }
        CatalogueInput::MoreChats => {
            state.query["chatLimit"] = json!(state.query["chatLimit"].as_u64().unwrap_or(5) + 10);
            effects.titles = state.connected;
        }
        CatalogueInput::MoreProjectThreads(id) => {
            state.query["projectThreadLimits"][&id] = json!(
                state.query["projectThreadLimits"][&id]
                    .as_u64()
                    .unwrap_or(5)
                    + 10
            );
            effects.titles = state.connected;
        }
        CatalogueInput::Visible(visible) => {
            if let Some(id) = &visible {
                state.indicators.unread.remove(id);
            }
            state.visible = visible;
        }
        _ => {}
    }
    if effects.titles {
        state.list_generation += 1;
    }
    if effects.models {
        state.model_generation += 1;
    }
    (state, effects)
}

/// Immutable I/O lease. Catalogue decisions live in reduce_catalogue, not here.
pub(super) struct Connection {
    pub(super) remote: String,
    pub(super) rpc: Rpc,
    pub(super) state: Entity<Catalogue>,
    events: async_channel::Sender<Event>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.rpc.close();
    }
}

pub(super) fn acquire(
    remote: String,
    leases: &mut HashMap<String, Weak<Connection>>,
    events: &async_channel::Sender<Event>,
    cx: &mut App,
) -> Rc<Connection> {
    if let Some(connection) = leases.get(&remote).and_then(Weak::upgrade) {
        return connection;
    }
    leases.retain(|_, value| value.strong_count() > 0);
    let state = cx.new(|_| Catalogue {
        query: json!({"projectLimit":10,"chatLimit":5,"projectThreadLimits":{},"searchTerm":""}),
        ..Default::default()
    });
    let id = state.entity_id();
    let tx = events.clone();
    let target = if remote.is_empty() {
        json!({"target":"local"})
    } else {
        json!({"target":"remote","profileId":remote})
    };
    let rpc = Rpc::connect(
        platform::state_dir().join("host.sock"),
        target,
        move |event| {
            let _ = tx.send_blocking(Event::Catalogue(id, CatalogueInput::Rpc(event)));
        },
    );
    let connection = Rc::new(Connection {
        remote,
        rpc,
        state,
        events: events.clone(),
    });
    leases.insert(connection.remote.clone(), Rc::downgrade(&connection));
    connection
}

pub(super) fn send(connection: &Connection, input: CatalogueInput) {
    let _ = connection
        .events
        .send_blocking(Event::Catalogue(connection.state.entity_id(), input));
}

pub(super) fn receive(connection: &Connection, input: CatalogueInput, cx: &mut App) {
    let effects = connection.state.update(cx, |state, cx| {
        let (next, mut effects) = reduce_catalogue(std::mem::take(state), input);
        *state = next;
        if let Some(notice) = effects.notice.take() {
            cx.emit(notice);
        }
        cx.notify();
        effects
    });
    let state = connection.state.read(cx);
    let id: EntityId = connection.state.entity_id();
    if effects.titles {
        let generation = state.list_generation;
        let query = state.query.clone();
        let events = connection.events.clone();
        connection.rpc.agent_async(
            move |client| async move { client.list_threads(&query).await },
            move |result| {
                let _ = events.send_blocking(Event::Catalogue(
                    id,
                    CatalogueInput::Titles(generation, result),
                ));
            },
        );
    }
    if effects.models {
        let generation = state.model_generation;
        let events = connection.events.clone();
        connection.rpc.agent_async(
            |client| async move { client.models().await },
            move |result| {
                let _ = events.send_blocking(Event::Catalogue(
                    id,
                    CatalogueInput::Models(generation, result),
                ));
            },
        );
    }
}

#[derive(Default)]
pub(super) struct TaskIndicators {
    pub(super) active: HashMap<String, bool>,
    pub(super) unread: HashSet<String>,
}
impl TaskIndicators {
    pub(super) fn is_active(&self, thread: &Value) -> bool {
        self.active
            .get(text(thread, "id"))
            .copied()
            .unwrap_or(thread["status"]["type"] == "active")
    }
}
fn reduce_indicators(
    mut state: TaskIndicators,
    message: &Value,
    visible: Option<&str>,
) -> TaskIndicators {
    let params = &message["params"];
    let Some(id) = params["threadId"].as_str().filter(|id| !id.is_empty()) else {
        return state;
    };
    let active = match text(message, "method") {
        "thread/status/changed" => params["status"]["type"] == "active",
        "turn/started" => true,
        "turn/completed" => {
            if params["turn"]["status"] == "completed" && visible != Some(id) {
                state.unread.insert(id.to_owned());
            }
            false
        }
        _ => return state,
    };
    if active {
        state.unread.remove(id);
    }
    if let Some(previous) = state.active.get_mut(id) {
        *previous = active;
    } else {
        state.active.insert(id.to_owned(), active);
    }
    state
}

#[cfg(test)]
mod task_indicator_tests {
    use super::{TaskIndicators, reduce_indicators};
    use serde_json::json;

    #[test]
    fn live_lifecycle_overrides_stale_list_and_marks_unseen_success() {
        let idle = json!({"id":"a", "status":{"type":"idle"}});
        let active = json!({"id":"a", "status":{"type":"active"}});
        let mut indicators = TaskIndicators::default();
        assert!(indicators.is_active(&active));
        assert!(!indicators.is_active(&idle));
        indicators = reduce_indicators(
            indicators,
            &json!({"method":"turn/started", "params":{"threadId":"a"}}),
            None,
        );
        assert!(indicators.is_active(&idle));
        indicators = reduce_indicators(
            indicators,
            &json!({"method":"turn/completed", "params":{"threadId":"a", "turn":{"status":"completed"}}}),
            Some("b"),
        );
        assert!(!indicators.is_active(&active));
        assert!(indicators.unread.contains("a"));
        indicators = reduce_indicators(
            indicators,
            &json!({"method":"thread/status/changed", "params":{"threadId":"a", "status":{"type":"idle"}}}),
            None,
        );
        assert!(indicators.unread.contains("a"));
        indicators = reduce_indicators(
            indicators,
            &json!({"method":"thread/status/changed", "params":{"threadId":"a", "status":{"type":"active"}}}),
            None,
        );
        assert!(indicators.is_active(&idle));
        assert!(indicators.unread.is_empty());
    }

    #[test]
    fn visible_success_and_unsuccessful_turns_do_not_mark_unread() {
        let mut indicators = TaskIndicators::default();
        for (status, visible) in [
            ("completed", Some("a")),
            ("failed", None),
            ("interrupted", None),
        ] {
            indicators = reduce_indicators(
                indicators,
                &json!({"method":"turn/completed", "params":{"threadId":"a", "turn":{"status":status}}}),
                visible,
            );
            assert!(indicators.unread.is_empty());
        }
        indicators = reduce_indicators(
            indicators,
            &json!({"method":"item/completed", "params":{"threadId":"a"}}),
            None,
        );
        assert!(indicators.unread.is_empty());
    }
}
