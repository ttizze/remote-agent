use std::{borrow::Cow, path::Path, sync::Arc, time::Duration};

use host_protocol::{RpcPeer, RpcPeerError, api};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// A lease on one connected peer. Connection replacement creates a new client;
/// in-flight work never migrates to a different authenticated connection.
#[derive(Clone)]
pub struct AgentClient {
    peer: Arc<RpcPeer>,
    deadline: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Transport(#[from] RpcPeerError),
    #[error("{0}")]
    Remote(Value),
    #[error("invalid agent response: {0}")]
    InvalidResponse(String),
    #[error("Invalid {method} response: {message}")]
    InvalidPayload {
        method: &'static str,
        message: String,
        raw: Value,
    },
}

impl AgentError {
    pub fn into_native_error(self) -> String {
        let message = self.to_string();
        let raw = match self {
            Self::Remote(raw) | Self::InvalidPayload { raw, .. } => Some(raw),
            _ => None,
        };
        json!({"message":message,"rawError":raw}).to_string()
    }
}

impl AgentClient {
    pub fn new(peer: Arc<RpcPeer>, deadline: Duration) -> Self {
        Self { peer, deadline }
    }

    pub async fn transcribe(&self, audio: &str) -> Result<String, AgentError> {
        #[derive(Serialize)]
        struct Audio<'a> {
            audio: &'a str,
        }
        let mut result: Value = self
            .request("host/dictation/transcribe", &Audio { audio })
            .await?;
        if !result["text"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
        {
            return invalid("音声を認識できませんでした。もう一度録音してください。");
        }
        let Value::String(text) = result["text"].take() else {
            unreachable!()
        };
        Ok(text)
    }

    pub async fn respond(
        &self,
        request: &Value,
        answer: RequestAnswer<'_>,
    ) -> Result<(), AgentError> {
        let line = response_line(request, answer).map_err(AgentError::InvalidResponse)?;
        self.peer.send_raw(&line).await?;
        Ok(())
    }

    pub async fn list_threads(&self, query: &Value) -> Result<Value, AgentError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Titles<'a> {
            title_only: bool,
            #[serde(flatten)]
            query: &'a Value,
        }
        let result: Value = self
            .request(
                "host/thread/list",
                &Titles {
                    title_only: true,
                    query,
                },
            )
            .await?;
        checked(result, "host/thread/list", |result| {
            for field in ["data", "projects", "moreProjectIds"] {
                if !result[field].is_array() {
                    return invalid(format!("{field} must be an array"));
                }
            }
            for field in ["hasMoreChats", "hasMoreProjects"] {
                if !result[field].is_boolean() {
                    return invalid(format!("{field} must be a boolean"));
                }
            }
            for field in ["data", "projects"] {
                for entry in result[field].as_array().unwrap() {
                    required_id(entry, "id")?;
                }
            }
            if result["moreProjectIds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| !id.is_string())
            {
                return invalid("project ID must be a string");
            }
            Ok(())
        })
    }

    pub async fn read_thread(
        &self,
        thread_id: &str,
        defer_item_details: bool,
    ) -> Result<Value, AgentError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.read_thread_with(thread_id.to_owned(), defer_item_details, move |result| {
            let _ = tx.send(result);
        })
        .await;
        rx.await
            .map_err(|_| RpcPeerError::ConnectionClosed("read completion stopped".into()))?
    }

    /// The reader invokes completion before dispatching the next notification.
    /// UI adapters enqueue the snapshot here; async callers use read_thread and
    /// retain their own lifecycle barrier until they install the returned state.
    pub async fn read_thread_with(
        &self,
        thread_id: String,
        defer_item_details: bool,
        done: impl FnOnce(Result<Value, AgentError>) + Send + 'static,
    ) {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Read<'a> {
            thread_id: &'a str,
            include_turns: bool,
            paginate_history: bool,
            defer_item_details: bool,
        }
        let line = match request_line(
            "host/thread/read",
            &Read {
                thread_id: &thread_id,
                include_turns: true,
                paginate_history: true,
                defer_item_details,
            },
        ) {
            Ok(line) => line,
            Err(error) => {
                done(Err(error));
                return;
            }
        };
        self.peer
            .request_with(&line, self.deadline, move |reply| {
                let result = reply
                    .map_err(AgentError::from)
                    .and_then(|raw| decode_response(&raw))
                    .and_then(|value| {
                        checked(value, "host/thread/read", |value| {
                            validate_thread(value, Some(&thread_id))
                        })
                    });
                done(result);
            })
            .await;
    }

    pub async fn read_older(
        &self,
        thread_id: &str,
        cursor: Option<&str>,
        turn_id: Option<&str>,
        defer_item_details: bool,
    ) -> Result<Value, AgentError> {
        let method = if turn_id.is_some() {
            "host/thread/items/list"
        } else {
            "host/thread/turns/list"
        };
        let result = self.request(method, &json!({"threadId":thread_id,"cursor":cursor,"turnId":turn_id,"deferItemDetails":defer_item_details})).await?;
        checked(result, method, |result| {
            validate_thread(result, Some(thread_id))?;
            if let Some(turn_id) = turn_id {
                let turns = result["thread"]["turns"].as_array();
                if !turns.is_some_and(|turns| turns.len() == 1 && turns[0]["id"] == turn_id) {
                    return invalid("turn ID does not match");
                }
            }
            Ok(())
        })
    }

    pub async fn watch_thread(
        &self,
        thread_id: &str,
        watch_key: u64,
        watch_id: u64,
        path: &str,
    ) -> Result<(), AgentError> {
        self.request::<_, Empty>(
            "host/thread/watch",
            &json!({"threadId":thread_id,"watchKey":watch_key,"watchId":watch_id,"path":path}),
        )
        .await
        .map(|_| ())
    }

    pub async fn unwatch_thread(&self, watch_key: u64, watch_id: u64) -> Result<(), AgentError> {
        self.request::<_, Empty>(
            "host/thread/unwatch",
            &json!({"watchKey":watch_key,"watchId":watch_id}),
        )
        .await
        .map(|_| ())
    }

    /// Traverse both turn and item cursors. Keep image values only while paging;
    /// source strings move out of the decoded JSON without copying their bodies.
    pub async fn session_images(&self, thread_id: &str) -> Result<Vec<SessionImage>, AgentError> {
        use crate::conversation::{self, Conversation, text};
        fn retain_images(thread: &mut Value) {
            if let Some(turns) = thread["turns"].as_array_mut() {
                for turn in turns {
                    if let Some(items) = turn["items"].as_array_mut() {
                        items.retain(|item| item["type"] == "imageGeneration");
                        for item in items {
                            if !text(item, "savedPath").trim().is_empty() {
                                item.as_object_mut().unwrap().remove("result");
                            }
                        }
                    }
                }
            }
        }
        let mut initial = self.read_thread(thread_id, true).await?;
        let mut state = Conversation {
            thread: initial["thread"].take(),
            ..Default::default()
        };
        retain_images(&mut state.thread);
        let mut cursors = std::collections::HashSet::new();
        while let Some(page) = state.older_page() {
            if !cursors.insert((page.turn.clone(), page.cursor.to_string())) {
                return invalid("履歴カーソルが進みませんでした");
            }
            let mut incoming = self
                .read_older(thread_id, page.cursor.as_str(), page.turn.as_deref(), true)
                .await?;
            retain_images(&mut incoming["thread"]);
            let (next, result) = conversation::merge_older(state, incoming, &page);
            state = next;
            result.map_err(AgentError::InvalidResponse)?;
        }
        let mut images = Vec::new();
        if let Value::Array(turns) = state.thread["turns"].take() {
            for mut turn in turns {
                if let Value::Array(items) = turn["items"].take() {
                    for mut item in items {
                        let encoded = text(&item, "savedPath").trim().is_empty();
                        if let Value::String(source) =
                            item[if encoded { "result" } else { "savedPath" }].take()
                        {
                            if !source.trim().is_empty() {
                                images.push(SessionImage { source, encoded });
                            }
                        }
                    }
                }
            }
        }
        // Keep the newest occurrence of each source, in conversation order.
        let keep = {
            let mut seen = std::collections::HashSet::new();
            let mut keep = vec![false; images.len()];
            for (index, image) in images.iter().enumerate().rev() {
                keep[index] = seen.insert((image.source.as_str(), image.encoded));
            }
            keep
        };
        let mut keep = keep.into_iter();
        images.retain(|_| keep.next().unwrap());
        Ok(images)
    }

    pub async fn read_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
    ) -> Result<Value, AgentError> {
        let result: Value = self
            .request(
                "host/thread/item/read",
                &json!({"threadId":thread_id,"turnId":turn_id,"itemId":item_id}),
            )
            .await?;
        checked(result, "host/thread/item/read", |result| {
            if required_id(&result["item"], "id")? != item_id {
                return invalid("item ID does not match");
            }
            Ok(())
        })
    }

    pub async fn start_thread(&self, cwd: &str, model: Option<&str>) -> Result<Value, AgentError> {
        let mut params = json!({});
        if !cwd.trim().is_empty() {
            params["cwd"] = cwd.into();
        }
        if let Some(model) = model {
            params["model"] = model.into();
        }
        let result = self.request("host/thread/start", &params).await?;
        checked(result, "host/thread/start", |result| {
            validate_thread(result, None)
        })
    }

    /// Execute the shared submission decision without retaining client settings.
    pub async fn send_turn(
        &self,
        thread_id: &str,
        plan: conversation_presentation::state::SendPlan,
        input: &[Value],
        client_user_message_id: &str,
        options: TurnOptions<'_>,
    ) -> Result<Option<String>, AgentError> {
        use conversation_presentation::state::SendPlan;
        let params = InputParams {
            thread_id,
            input,
            client_user_message_id,
            expected_turn_id: None,
            options: TurnOptions::default(),
        };
        match plan {
            SendPlan::Steer { turn_id } => {
                self.request::<_, serde::de::IgnoredAny>(
                    "turn/steer",
                    &InputParams {
                        expected_turn_id: Some(&turn_id),
                        ..params
                    },
                )
                .await?;
                Ok(Some(turn_id))
            }
            SendPlan::Queue => {
                let result = self.request("thread/queue/add", &params).await?;
                checked(result, "thread/queue/add", |result| {
                    required_id(&result["queuedSubmission"], "id")
                        .map(|_| ())
                        .or_else(|_| invalid("queued submission ID is missing"))
                })?;
                Ok(None)
            }
            SendPlan::Start { cwd, resume } => {
                if resume {
                    #[derive(Serialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Resume<'a> {
                        thread_id: &'a str,
                        cwd: &'a str,
                    }
                    self.request::<_, serde::de::IgnoredAny>(
                        "thread/resume",
                        &Resume {
                            thread_id,
                            cwd: &cwd,
                        },
                    )
                    .await?;
                }
                let mut result: Value = self
                    .request("turn/start", &InputParams { options, ..params })
                    .await?;
                let id = result
                    .get("turnId")
                    .and_then(Value::as_str)
                    .or_else(|| result["turn"]["id"].as_str());
                if id.is_none_or(|id| id.trim().is_empty()) {
                    return Err(AgentError::InvalidPayload {
                        method: "turn/start",
                        message: "turn ID is missing".into(),
                        raw: result,
                    });
                }
                let Value::String(id) = (if result["turnId"].is_string() {
                    result["turnId"].take()
                } else {
                    result["turn"]["id"].take()
                }) else {
                    unreachable!("validated turn ID")
                };
                Ok(Some(id))
            }
            SendPlan::Reject { message } => invalid(message),
        }
    }

    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), AgentError> {
        self.request::<_, Value>(
            "turn/interrupt",
            &json!({"threadId":thread_id,"turnId":turn_id}),
        )
        .await
        .map(|_| ())
    }

    pub async fn models(&self) -> Result<Vec<Value>, AgentError> {
        #[derive(Serialize)]
        struct Page<'a> {
            limit: u32,
            #[serde(skip_serializing_if = "Option::is_none")]
            cursor: Option<&'a str>,
        }
        let mut models = Vec::new();
        let mut positions = std::collections::HashMap::new();
        let mut cursors = std::collections::HashSet::new();
        let mut cursor = None::<String>;
        loop {
            let page: Value = self
                .request(
                    "model/list",
                    &Page {
                        limit: 100,
                        cursor: cursor.as_deref(),
                    },
                )
                .await?;
            let mut page = checked(page, "model/list", |page| {
                let entries = page["data"].as_array().ok_or_else(|| {
                    AgentError::InvalidResponse("model data must be an array".into())
                })?;
                for entry in entries {
                    for field in ["id", "model", "displayName", "defaultReasoningEffort"] {
                        if !entry[field].is_string() {
                            return invalid(format!("missing {field}"));
                        }
                    }
                    if !entry["supportedReasoningEfforts"]
                        .as_array()
                        .is_some_and(|efforts| {
                            efforts
                                .iter()
                                .all(|effort| effort["reasoningEffort"].is_string())
                        })
                    {
                        return invalid("invalid supported reasoning efforts");
                    }
                }
                Ok(())
            })?;
            let Value::Array(entries) = page["data"].take() else {
                unreachable!("validated model array")
            };
            for entry in entries {
                let id = entry["id"].as_str().unwrap();
                match positions.entry(id.to_owned()) {
                    std::collections::hash_map::Entry::Occupied(position) => {
                        models[*position.get()] = entry
                    }
                    std::collections::hash_map::Entry::Vacant(position) => {
                        position.insert(models.len());
                        models.push(entry);
                    }
                }
            }
            cursor = page["nextCursor"]
                .as_str()
                .filter(|cursor| !cursor.trim().is_empty())
                .map(str::to_owned);
            let Some(next) = &cursor else {
                return Ok(models);
            };
            if !cursors.insert(next.clone()) || cursors.len() >= 64 {
                return Err(AgentError::InvalidResponse(
                    "モデル一覧の続きを取得できませんでした".into(),
                ));
            }
        }
    }

    pub async fn list_files(&self, path: &Path) -> Result<api::FileList, AgentError> {
        self.request(api::FILE_LIST, &api::FilePathParams { path: path.into() })
            .await
    }

    pub async fn read_file(&self, path: &Path) -> Result<api::FileDocument<'static>, AgentError> {
        self.request(api::FILE_READ, &api::FilePathParams { path: path.into() })
            .await
    }

    pub async fn write_file(
        &self,
        path: &Path,
        revision: &str,
        text: &str,
    ) -> Result<api::FileDocument<'static>, AgentError> {
        self.request(
            api::FILE_WRITE,
            &api::FileWriteParams {
                path: path.into(),
                revision: revision.into(),
                text: text.into(),
            },
        )
        .await
    }

    pub async fn review_workspace(&self, cwd: &str) -> Result<api::WorkspaceReview, AgentError> {
        self.request(
            api::WORKSPACE_REVIEW,
            &api::WorkspaceReviewParams { cwd: cwd.into() },
        )
        .await
    }

    pub async fn worktree_settings(&self) -> Result<api::WorktreeSettings, AgentError> {
        self.request(api::WORKTREE_SETTINGS_READ, &Empty {}).await
    }

    pub async fn update_worktree_settings(
        &self,
        settings: &api::WorktreeSettings,
    ) -> Result<api::WorktreeSettings, AgentError> {
        self.request(api::WORKTREE_SETTINGS_UPDATE, settings).await
    }

    pub async fn accounts(&self) -> Result<api::AccountList<'static>, AgentError> {
        self.request(api::ACCOUNT_LIST, &Empty {}).await
    }

    pub async fn select_account(
        &self,
        account_id: &str,
    ) -> Result<api::AccountSelection<'static>, AgentError> {
        self.request(
            api::ACCOUNT_SELECT,
            &api::AccountSelectParams { account_id },
        )
        .await
    }

    pub async fn start_account_login(&self) -> Result<api::AccountLogin<'static>, AgentError> {
        self.request(api::ACCOUNT_LOGIN_START, &Empty {}).await
    }

    pub async fn account_login_status(
        &self,
        login_id: &str,
    ) -> Result<api::AccountLoginStatus<'static>, AgentError> {
        self.request(
            api::ACCOUNT_LOGIN_STATUS,
            &api::AccountLoginParams { login_id },
        )
        .await
    }

    pub async fn cancel_account_login(&self, login_id: &str) -> Result<(), AgentError> {
        self.request::<_, Empty>(
            api::ACCOUNT_LOGIN_CANCEL,
            &api::AccountLoginParams { login_id },
        )
        .await
        .map(|_| ())
    }

    pub async fn fork_thread(
        &self,
        thread_id: &str,
        last_turn_id: &str,
    ) -> Result<Value, AgentError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Fork<'a> {
            thread_id: &'a str,
            last_turn_id: &'a str,
            exclude_turns: bool,
        }
        let result: Value = self
            .request(
                "thread/fork",
                &Fork {
                    thread_id,
                    last_turn_id,
                    exclude_turns: true,
                },
            )
            .await?;
        checked(result, "thread/fork", |result| {
            validate_thread(result, None)
        })
    }

    /// Complete in reader order before the following notification is dispatched.
    pub async fn request_with<P: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        done: impl FnOnce(Result<R, AgentError>) + Send + 'static,
    ) {
        let line = match request_line(method, params) {
            Ok(line) => line,
            Err(error) => {
                done(Err(error));
                return;
            }
        };
        self.peer
            .request_with(&line, self.deadline, move |reply| {
                done(
                    reply
                        .map_err(AgentError::from)
                        .and_then(|raw| decode_response(&raw)),
                );
            })
            .await;
    }

    pub async fn request<P: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, AgentError> {
        let line = request_line(method, params)?;
        let raw = self.peer.request_raw(&line, self.deadline).await?;
        decode_response(&raw)
    }
}

/// UI input only. The original request supplies wire IDs, decisions and permissions.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RequestAnswer<'a> {
    Decision {
        index: usize,
    },
    Permissions {
        allow: bool,
    },
    Answers {
        answers: std::collections::BTreeMap<String, String>,
    },
    Raw {
        #[serde(borrow)]
        json: Cow<'a, str>,
    },
}

fn response_line(request: &Value, answer: RequestAnswer<'_>) -> Result<String, String> {
    #[derive(Serialize)]
    struct Reply<'a, T> {
        id: &'a Value,
        result: T,
    }
    #[derive(Serialize)]
    struct Decision<T> {
        decision: T,
    }
    #[derive(Serialize)]
    struct Permissions<'a> {
        permissions: &'a Value,
        scope: &'static str,
    }
    #[derive(Serialize)]
    struct Answers<T> {
        answers: T,
    }
    struct QuestionAnswers<'a> {
        questions: &'a Value,
        answers: &'a std::collections::BTreeMap<String, String>,
    }
    impl Serialize for QuestionAnswers<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut map = serializer.serialize_map(None)?;
            for question in self
                .questions
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let Some(key) = question["id"].as_str() else {
                    continue;
                };
                let value = self
                    .answers
                    .get(key)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| serde::ser::Error::custom("すべての質問に回答してください"))?;
                map.serialize_entry(key, &Answers { answers: [value] })?;
            }
            map.end()
        }
    }
    let id = &request["id"];
    let params = &request["params"];
    match answer {
        RequestAnswer::Decision { index } => {
            let decision = conversation_presentation::requests::decision_at(params, index)
                .ok_or("承認の選択肢が無効です")?;
            serde_json::to_string(&Reply {
                id,
                result: Decision { decision },
            })
        }
        RequestAnswer::Permissions { allow } => {
            let empty = Value::Object(Default::default());
            let permissions = if allow {
                params.get("permissions").unwrap_or(&empty)
            } else {
                &empty
            };
            serde_json::to_string(&Reply {
                id,
                result: Permissions {
                    permissions,
                    scope: "turn",
                },
            })
        }
        RequestAnswer::Answers { answers } => serde_json::to_string(&Reply {
            id,
            result: Answers {
                answers: QuestionAnswers {
                    questions: &params["questions"],
                    answers: &answers,
                },
            },
        }),
        RequestAnswer::Raw { json } => {
            let result: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            serde_json::to_string(&Reply { id, result })
        }
    }
    .map_err(|e| e.to_string())
}

fn request_line<P: Serialize + ?Sized>(method: &str, params: &P) -> Result<String, AgentError> {
    #[derive(Serialize)]
    struct Request<'a, P: ?Sized> {
        id: u8,
        method: &'a str,
        params: &'a P,
    }
    serde_json::to_string(&Request {
        id: 0,
        method,
        params,
    })
    .map_err(|error| AgentError::InvalidResponse(error.to_string()))
}

fn decode_response<R: DeserializeOwned>(raw: &str) -> Result<R, AgentError> {
    fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        T::deserialize(deserializer).map(Some)
    }
    #[derive(Deserialize)]
    #[serde(bound(deserialize = "R: Deserialize<'de>"))]
    struct Response<R> {
        #[serde(default, deserialize_with = "present")]
        result: Option<R>,
        error: Option<Value>,
    }
    let response: Response<R> = serde_json::from_str(raw)
        .map_err(|error| AgentError::InvalidResponse(error.to_string()))?;
    if let Some(error) = response.error {
        return Err(AgentError::Remote(error));
    }
    response
        .result
        .ok_or_else(|| AgentError::InvalidResponse("result is missing".into()))
}

#[derive(Serialize, Deserialize)]
struct Empty {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InputParams<'a> {
    thread_id: &'a str,
    input: &'a [Value],
    client_user_message_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_turn_id: Option<&'a str>,
    #[serde(flatten)]
    options: TurnOptions<'a>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment<'a> {
    #[serde(borrow)]
    pub path: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    pub is_image: bool,
}

pub fn message_input<'a>(
    text: &str,
    attachments: impl IntoIterator<Item = Attachment<'a>>,
) -> Vec<Value> {
    let attachments = attachments.into_iter();
    let mut input =
        Vec::with_capacity(usize::from(!text.trim().is_empty()) + attachments.size_hint().0);
    if !text.trim().is_empty() {
        input.push(json!({"type":"text","text":text,"text_elements":[]}));
    }
    for attachment in attachments {
        input.push(if attachment.is_image {
            json!({"type":"localImage","path":attachment.path})
        } else {
            json!({"type":"mention","path":attachment.path,"name":attachment.name})
        });
    }
    input
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnOptions<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier_for_turn: Option<&'a str>,
}

fn invalid<T>(message: impl Into<String>) -> Result<T, AgentError> {
    Err(AgentError::InvalidResponse(message.into()))
}

fn required_id<'a>(value: &'a Value, field: &str) -> Result<&'a str, AgentError> {
    value[field]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| AgentError::InvalidResponse(format!("{field} is missing")))
}

fn validate_thread(result: &Value, expected: Option<&str>) -> Result<(), AgentError> {
    let thread = result
        .get("thread")
        .ok_or_else(|| AgentError::InvalidResponse("thread is missing".into()))?;
    let id = thread
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| AgentError::InvalidResponse("thread ID is missing".into()))?;
    if expected.is_some_and(|expected| id != expected) {
        return Err(AgentError::InvalidResponse(
            "thread ID does not match".into(),
        ));
    }
    if let Some(turns) = thread.get("turns") {
        let turns = turns
            .as_array()
            .ok_or_else(|| AgentError::InvalidResponse("turns must be an array".into()))?;
        for turn in turns {
            if turn
                .get("id")
                .or_else(|| turn.get("turnId"))
                .and_then(Value::as_str)
                .is_none_or(|id| id.trim().is_empty())
            {
                return Err(AgentError::InvalidResponse("turn ID is missing".into()));
            }
        }
    }
    Ok(())
}

fn checked(
    result: Value,
    method: &'static str,
    validate: impl FnOnce(&Value) -> Result<(), AgentError>,
) -> Result<Value, AgentError> {
    match validate(&result) {
        Ok(()) => Ok(result),
        Err(error) => {
            let message = match error {
                AgentError::InvalidResponse(message) => message,
                other => other.to_string(),
            };
            Err(AgentError::InvalidPayload {
                method,
                message,
                raw: result,
            })
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}
