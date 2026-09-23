//! The Bex client: typed operations on independent QUIC streams.
mod connection;
use crate::state::operations::ReadThread;
use crate::{models::ThreadResponse, peer::PeerError};
pub(crate) use connection::{BLOB, CALL, CLOSE, EVENTS};
pub use connection::{Client, HostPeer, HostRequest, Updates};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// The method, parameters and result are one contract.
pub trait RpcMethod: Serialize {
    type Contract: crate::protocol::contracts::Contract<Output = Self::Output>;
    type Output: DeserializeOwned + Serialize;
    fn method(&self) -> &'static str {
        <Self::Contract as crate::protocol::contracts::Contract>::METHOD
    }
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError>;
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        self.params()
            .map(<Self::Contract as crate::protocol::contracts::Contract>::call)
    }
    fn subscription(_output: &mut Self::Output, _id: uuid::Uuid) {}
    fn validate(&self, _output: &Self::Output) -> Result<(), &'static str> {
        Ok(())
    }
}
macro_rules! rpc_contract {
    ($variant:ident) => {
        type Contract = $crate::protocol::contracts::$variant;
        type Output = <$crate::protocol::contracts::$variant as $crate::protocol::contracts::Contract>::Output;
    };
}
pub(crate) use rpc_contract;
macro_rules! rpc_method {
    ($name:ty, $variant:ident, |$this:ident| $params:expr) => {
        impl $crate::client::RpcMethod for $name {
            $crate::client::rpc_contract!($variant);
            fn params(&$this) -> Result<<Self::Contract as $crate::protocol::contracts::Contract>::Params, $crate::peer::PeerError> {
                Ok($params)
            }
        }
    };
}
pub(crate) use rpc_method;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pair {
    pub invitation: uuid::Uuid,
}

#[derive(Debug, Serialize, Clone)]
pub struct ReadHostStatus {}
rpc_method!(ReadHostStatus, HostStatus, |self| crate::models::Empty {});

#[derive(Debug, Serialize, Clone)]
pub struct ListRemoteHosts {}
rpc_method!(ListRemoteHosts, ListRemotes, |self| crate::models::Empty {});
#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct RegisterRemoteHost {
    pub ticket: String,
    pub name: String,
}

impl Client {
    pub(crate) async fn open_subscription<O: RpcMethod>(
        &self,
        operation: &O,
    ) -> Result<Option<(O::Output, Updates, uuid::Uuid)>, PeerError> {
        if operation.method() != "host/session/open" {
            return Ok(None);
        }
        let (mut output, updates) = self
            .request_stream::<O::Output>(&operation.request()?)
            .await?;
        validate_output(operation, &output)?;
        let id = uuid::Uuid::new_v4();
        O::subscription(&mut output, id);
        Ok(Some((output, updates, id)))
    }
    pub async fn call<O: RpcMethod>(&self, operation: &O) -> Result<O::Output, PeerError> {
        let output = self.request(&operation.request()?).await?;
        validate_output(operation, &output)?;
        Ok(output)
    }
}

fn validate_output<O: RpcMethod>(operation: &O, output: &O::Output) -> Result<(), PeerError> {
    operation
        .validate(output)
        .map_err(|reason| PeerError::InvalidResponse {
            method: operation.method().into(),
            reason: reason.into(),
            sequence: None,
            raw: serde_json::to_string(output).expect("wire output serializes"),
        })
}

// The input schema is shared; only JSON's tagged representation differs from Postcard.
macro_rules! inputs {
    ($($variant:ident { $($field:ident: $ty:ty),* $(,)? }),* $(,)?) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub enum Input { $($variant { $($field: $ty),* }),* }
        mod input_json {
            use super::*;
            #[derive(Serialize, Deserialize)]
            #[serde(tag = "type", rename_all = "camelCase")]
            enum JsonInput<T> { $($variant { $($field: T),* }),* }
            pub fn serialize<S: serde::Serializer>(input: &[Input], serializer: S) -> Result<S::Ok, S::Error> {
                if !serializer.is_human_readable() { return input.serialize(serializer); }
                serializer.collect_seq(input.iter().map(|item| match item {
                    $(Input::$variant { $($field),* } => JsonInput::$variant { $($field),* }),*
                }))
            }
            pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Input>, D::Error> {
                if !deserializer.is_human_readable() { return Vec::<Input>::deserialize(deserializer); }
                Ok(Vec::<JsonInput<String>>::deserialize(deserializer)?.into_iter().map(|item| match item {
                    $(JsonInput::$variant { $($field),* } => Input::$variant { $($field),* }),*
                }).collect())
            }
        }
    }
}
inputs! {
    Text { text: String },
    Skill { name: String, path: String },
    LocalImage { path: String },
    Mention { path: String, name: String },
}
impl Input {
    /// Provider-shaped content stored in a user message.
    pub fn content(input: &[Self]) -> Value {
        input_json::serialize(input, serde_json::value::Serializer).expect("input serializes")
    }
}

#[derive(Debug, Serialize, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTurn {
    pub thread_id: String,
    pub client_user_message_id: String,
    #[serde(with = "input_json")]
    pub input: Vec<Input>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(rename = "serviceTierForTurn")]
    pub service_tier: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct StartedTurn {
    pub turn: TurnIdentity,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct TurnIdentity {
    pub id: String,
}
impl StartTurn {
    pub(crate) fn validate(&self, output: &StartedTurn) -> Result<(), &'static str> {
        let id = &output.turn.id;
        if id.trim().is_empty() {
            Err("turn ID is missing")
        } else {
            Ok(())
        }
    }
}

pub(crate) fn validate_thread(
    output: &ThreadResponse,
    expected: Option<&str>,
) -> Result<(), &'static str> {
    let id = output
        .thread
        .id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or("thread ID is missing")?;
    if expected.is_some_and(|expected| id != expected) {
        Err("thread ID does not match")
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemResponse {
    pub item: crate::models::Item,
    pub transfer: Option<crate::models::TransferGrant>,
}
impl ItemResponse {
    pub async fn resolve(
        mut self,
        session: Option<&crate::transport::Session>,
    ) -> Result<Self, PeerError> {
        if let Some(grant) = self.transfer.take() {
            let session = session.ok_or_else(|| {
                PeerError::InvalidMessage("item transfer requires an iroh session".into())
            })?;
            let bytes = crate::transfers::download_bytes(grant, || async {
                session.open_stream().await.map_err(std::io::Error::other)
            })
            .await
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            let item: crate::models::Item = crate::protocol::decode(&bytes)
                .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            if item.id != self.item.id {
                return Err(PeerError::InvalidMessage(
                    "transferred item ID does not match".into(),
                ));
            }
            self.item = item;
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameThread {
    pub thread_id: String,
    pub name: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeThread {
    pub thread_id: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerTurn {
    pub thread_id: String,
    pub client_user_message_id: String,
    #[serde(with = "input_json")]
    pub input: Vec<Input>,
    pub expected_turn_id: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueTurn {
    pub thread_id: String,
    pub client_user_message_id: String,
    #[serde(with = "input_json")]
    pub input: Vec<Input>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedTurn {
    pub queued_submission: TurnIdentity,
}
impl QueueTurn {
    pub(crate) fn validate(&self, output: &QueuedTurn) -> Result<(), &'static str> {
        if output.queued_submission.id.trim().is_empty() {
            Err("queued submission ID is missing")
        } else {
            Ok(())
        }
    }
}

fn model_limit() -> usize {
    100
}
#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct ListModels {
    #[serde(default = "model_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPage {
    pub data: Vec<crate::models::Model>,
    pub next_cursor: Option<String>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub provider_errors: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "T: AsRef<[u8]>", deserialize = "T: From<Vec<u8>>"))]
pub struct Transcribe<T = Vec<u8>> {
    #[serde(with = "crate::protocol::bytes")]
    pub audio: T,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Transcription {
    pub text: String,
}
impl<T: AsRef<[u8]>> RpcMethod for Transcribe<T> {
    crate::client::rpc_contract!(Transcribe);
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(Transcribe {
            audio: self.audio.as_ref().to_vec(),
        })
    }
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub revision: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Account {
    pub id: String,
    pub provider: crate::session::ProviderKind,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub usage: Option<AccountUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AccountUsage {
    pub windows: Vec<UsageWindow>,
    pub fetched_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageWindow {
    pub label: String,
    pub remaining_percent: u32,
    pub resets_at: Option<i64>,
}

impl UsageWindow {
    pub fn from_used(label: String, used: f64, resets_at: Option<i64>) -> Option<Self> {
        used.is_finite().then(|| Self {
            label,
            remaining_percent: (100.0 - used.clamp(0.0, 100.0)).floor() as u32,
            resets_at,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub selected_id: Option<String>,
    pub selected_claude_id: Option<String>,
    pub error: Option<String>,
}

impl Accounts {
    pub fn is_selected(&self, account: &Account) -> bool {
        (match account.provider {
            crate::session::ProviderKind::Codex => self.selected_id.as_ref(),
            crate::session::ProviderKind::Claude => self.selected_claude_id.as_ref(),
        }) == Some(&account.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StartAccountLogin {
    pub provider: crate::session::ProviderKind,
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubmitAccountLogin {
    pub id: String,
    pub code: String,
}
impl std::fmt::Debug for SubmitAccountLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitAccountLogin")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelection {
    pub provider: crate::session::ProviderKind,
    pub selected_id: String,
    pub persistence_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AccountLogin {
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStatus {
    pub completed: bool,
    pub account_id: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum SubmissionTarget<'a> {
    Steer(&'a str),
    Queue,
    Start { cwd: &'a str, resume: bool },
}
pub fn submission_target<'a>(
    snapshot: Option<&'a crate::models::Thread>,
    listed: Option<&'a crate::models::Thread>,
    active: Option<bool>,
) -> Result<SubmissionTarget<'a>, PeerError> {
    // Current execution state takes precedence over unfinished historical turns.
    let active = active.or_else(|| {
        snapshot
            .and_then(|thread| thread.status.as_ref())
            .map(|status| status.kind == crate::models::ThreadStatusKind::Active)
    });
    if active != Some(false)
        && let Some(turn) = snapshot
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| {
                turns.iter().rev().find(|turn| {
                    turn.status.as_deref() == Some("inProgress") && !turn.id.trim().is_empty()
                })
            })
    {
        return Ok(SubmissionTarget::Steer(&turn.id));
    }
    if active.unwrap_or_else(|| {
        listed
            .and_then(|thread| thread.status.as_ref())
            .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::Active)
    }) {
        return Ok(SubmissionTarget::Queue);
    }
    let cwd = [snapshot, listed]
        .into_iter()
        .flatten()
        .filter_map(|thread| thread.cwd.as_deref())
        .find(|cwd| !cwd.trim().is_empty())
        .ok_or_else(|| PeerError::InvalidMessage("thread working directory is unknown".into()))?;
    let resume = snapshot.is_none_or(|thread| {
        thread
            .status
            .as_ref()
            .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::NotLoaded)
    });
    Ok(SubmissionTarget::Start { cwd, resume })
}

pub struct Submission<'a> {
    pub thread_id: &'a str,
    pub client_user_message_id: &'a str,
    pub input: &'a [Input],
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub service_tier: Option<&'a str>,
}
impl Client {
    pub async fn submit(
        &self,
        submission: &Submission<'_>,
        target: SubmissionTarget<'_>,
    ) -> Result<Option<String>, PeerError> {
        let Submission {
            thread_id,
            client_user_message_id,
            input,
            model,
            effort,
            service_tier,
        } = *submission;
        match target {
            SubmissionTarget::Steer(turn_id) => {
                self.call(&SteerTurn {
                    thread_id: thread_id.to_owned(),
                    client_user_message_id: client_user_message_id.to_owned(),
                    input: input.to_vec(),
                    expected_turn_id: turn_id.to_owned(),
                })
                .await?;
                Ok(Some(turn_id.into()))
            }
            SubmissionTarget::Queue => {
                self.call(&QueueTurn {
                    thread_id: thread_id.to_owned(),
                    client_user_message_id: client_user_message_id.to_owned(),
                    input: input.to_vec(),
                })
                .await?;
                Ok(None)
            }
            SubmissionTarget::Start { cwd, resume } => {
                if resume {
                    self.call(&ResumeThread {
                        thread_id: thread_id.to_owned(),
                        cwd: Some(cwd.to_owned()),
                    })
                    .await?;
                }
                let reply = self
                    .call(&StartTurn {
                        thread_id: thread_id.to_owned(),
                        client_user_message_id: client_user_message_id.to_owned(),
                        input: input.to_vec(),
                        model: model.map(str::to_owned),
                        effort: effort.map(str::to_owned),
                        service_tier: service_tier.map(str::to_owned),
                    })
                    .await?;
                let id = reply.turn.id;
                Ok(Some(id))
            }
        }
    }

    pub async fn models(&self) -> Result<ModelPage, PeerError> {
        let mut provider_errors = Map::<String, Value>::new();
        let mut models: Vec<crate::models::Model> = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .call(&ListModels {
                    limit: 100,
                    cursor: cursor.clone(),
                })
                .await?;
            if let Some(errors) = page.provider_errors {
                provider_errors.extend(errors);
            }
            for model in page.data {
                if let Some(index) = models.iter().position(|previous| previous.id == model.id) {
                    models[index] = model;
                } else {
                    models.push(model);
                }
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                return Ok(ModelPage {
                    data: models,
                    next_cursor: None,
                    provider_errors: (!provider_errors.is_empty()).then_some(provider_errors),
                });
            };
            if !seen.insert(next.clone()) {
                return Err(PeerError::InvalidMessage(
                    "model cursor did not advance".into(),
                ));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRequest {
    #[serde(default, rename = "deliveryState")]
    pub delivery_state: Option<crate::session::RequestDelivery>,
    #[serde(default, rename = "nativeRequestId")]
    #[serde(with = "crate::protocol::json")]
    pub native_request_id: Option<Value>,
    #[serde(with = "crate::protocol::json")]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub params: Map<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Answer {
    Decision {
        index: u32,
    },
    Permissions {
        allow: bool,
    },
    Questions {
        answers: std::collections::BTreeMap<String, String>,
    },
    Raw {
        value: Value,
    },
}
/// The UI and response validator use the same ordered choices.
pub fn approval_decisions(request: &ServerRequest) -> &[Value] {
    static DEFAULTS: std::sync::LazyLock<[Value; 4]> = std::sync::LazyLock::new(|| {
        ["accept", "acceptForSession", "decline", "cancel"].map(|value| Value::String(value.into()))
    });
    request
        .params
        .get("availableDecisions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(DEFAULTS.as_slice())
}
/// Validates a response before any bytes are queued. Unknown payloads remain intact.
pub fn answer_result(request: &ServerRequest, answer: &Answer) -> Result<Value, PeerError> {
    use serde_json::json;
    match answer {
        Answer::Decision { index } => {
            let choices = approval_decisions(request);
            let decision = choices
                .get(*index as usize)
                .ok_or_else(|| PeerError::InvalidMessage("invalid approval choice".into()))?;
            Ok(json!({"decision":decision}))
        }
        Answer::Permissions { allow } => {
            let permissions = if *allow {
                request
                    .params
                    .get("permissions")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("permissions object is missing".into())
                    })?
            } else {
                json!({})
            };
            Ok(json!({"permissions":permissions,"scope":"turn"}))
        }
        Answer::Questions { answers } => {
            #[derive(Deserialize)]
            struct Question {
                id: String,
            }
            let questions: Vec<Question> = serde_json::from_value(
                request
                    .params
                    .get("questions")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            let mut result = Map::new();
            for question in questions {
                let text = answers
                    .get(&question.id)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("every question requires an answer".into())
                    })?;
                result.insert(question.id, json!({"answers":[text]}));
            }
            Ok(json!({"answers":result}))
        }
        Answer::Raw { value } => Ok(value.clone()),
    }
}
/// Validate the complete wire answer before claiming a shared pending request.
pub fn validate_answer(request: &ServerRequest, result: &Value) -> Result<(), String> {
    if request.method == "item/tool/requestUserInput" {
        let questions = request
            .params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or("questions are unavailable")?;
        let answers = result["answers"]
            .as_object()
            .ok_or("answers are required")?;
        if answers.len() != questions.len() {
            return Err("every question requires one answer".into());
        }
        for question in questions {
            let id = question["id"].as_str().ok_or("question ID is missing")?;
            let values = answers
                .get(id)
                .and_then(|answer| answer["answers"].as_array())
                .ok_or("question answer is missing")?;
            if values.is_empty()
                || values
                    .iter()
                    .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
            {
                return Err("question answer is invalid".into());
            }
        }
    } else if request.method == "item/permissions/requestApproval" {
        let requested = request
            .params
            .get("permissions")
            .and_then(Value::as_object)
            .ok_or("permissions are unavailable")?;
        let granted = result["permissions"]
            .as_object()
            .ok_or("permissions are required")?;
        if result["scope"] != "turn"
            || granted
                .iter()
                .any(|(key, value)| requested.get(key) != Some(value))
        {
            return Err("answer grants unrequested permissions".into());
        }
    } else if matches!(
        request.method.as_str(),
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "claude/tool/requestApproval"
    ) {
        if !approval_decisions(request).contains(&result["decision"]) {
            return Err("invalid approval decision".into());
        }
    } else if request.method == "mcpServer/elicitation/request" {
        match result["action"].as_str() {
            Some("decline" | "cancel") if result["content"].is_null() => {}
            Some("accept")
                if matches!(
                    request.params.get("mode").and_then(Value::as_str),
                    Some("form" | "openai/form")
                ) && result["content"].is_object() => {}
            Some("accept")
                if request.params.get("mode").and_then(Value::as_str) == Some("url")
                    && result["content"].is_null() => {}
            _ => return Err("invalid MCP elicitation answer".into()),
        }
    } else if request.method == "item/tool/call" {
        if !result["success"].is_boolean()
            || result["contentItems"].as_array().is_none_or(|items| {
                items.iter().any(|item| match item["type"].as_str() {
                    Some("inputText") => !item["text"].is_string(),
                    Some("inputImage") => item["imageUrl"]
                        .as_str()
                        .is_none_or(|url| !url.starts_with("data:image/")),
                    _ => true,
                })
            })
        {
            return Err("invalid dynamic tool response".into());
        }
    } else {
        return Err("unsupported provider request cannot be approved".into());
    }
    Ok(())
}

impl Client {
    pub async fn respond(&self, request: &ServerRequest, answer: &Answer) -> Result<(), PeerError> {
        let result = answer_result(request, answer)?;
        validate_answer(request, &result).map_err(PeerError::InvalidMessage)?;
        self.request::<crate::models::Empty>(&crate::protocol::Call::AnswerSession(SessionAnswer {
            request_id: request.id.clone(),
            result,
        }))
        .await
        .map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}
impl Client {
    pub async fn session_images(
        &self,
        thread_id: &str,
        session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError> {
        // One bounded provider view; close this transient subscription before
        // returning images so it never replaces the Store's visible session.
        let read = ReadThread {
            limit: 1000,
            ..ReadThread::new(thread_id.to_owned())
        };
        let (opened, stream) = self
            .request_stream::<crate::session::OpenedSession>(&read.request()?)
            .await?;
        validate_output(&read, &opened)?;
        drop(stream);
        let thread = opened.response.thread;
        let mut images = Vec::new();
        let mut sources = std::collections::HashSet::new();
        let mut native_items = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for turn in thread.turns.as_deref().unwrap_or_default().iter().rev() {
            for item in turn.items.as_deref().unwrap_or_default().iter().rev() {
                if item.kind.as_deref() != Some("imageGeneration") || !native_items.insert(&item.id)
                {
                    continue;
                }
                let detail;
                let item = if turn
                    .deferred_item_ids
                    .as_ref()
                    .is_some_and(|ids| ids.contains(&item.id))
                {
                    detail = self
                        .call(&crate::state::operations::ReadItem {
                            thread_id: thread_id.into(),
                            turn_id: turn.id.clone(),
                            item_id: item.id.clone(),
                        })
                        .await?
                        .resolve(session)
                        .await?;
                    &detail.item
                } else {
                    item.as_ref()
                };
                let image = item
                    .saved_path
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                    .map(|path| (path, false))
                    .or_else(|| {
                        item.result
                            .as_ref()
                            .and_then(Value::as_str)
                            .filter(|data| !data.trim().is_empty())
                            .map(|data| (data, true))
                    });
                if let Some((source, encoded)) = image
                    && sources.insert(source.to_owned())
                {
                    bytes = bytes.saturating_add(source.len());
                    if bytes > 16 * 1024 * 1024 {
                        return Err(PeerError::InvalidMessage("画像一覧が16 MiBの上限に達しました。会話内の画像から個別に開いてください。".into()));
                    }
                    images.push(SessionImage {
                        source: source.into(),
                        encoded,
                    });
                }
            }
        }
        if thread.history_has_more == Some(true)
            || thread
                .history_read_state
                .as_ref()
                .is_some_and(|state| state.kind != crate::session::HistoryReadKind::Complete)
        {
            return Err(PeerError::InvalidMessage("履歴が部分取得のため、画像一覧を完全には取得できません。会話内の画像から個別に開いてください。".into()));
        }
        images.reverse();
        Ok(images)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

// Shared Host/Client request records; Store behavior lives in state::operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AddProject {
    pub cwd: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ListThreads {
    pub query: crate::models::ListQuery,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    #[serde(with = "crate::protocol::json")]
    pub request_id: Value,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub cwd: String,
    pub size: TerminalSize,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub path: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoutAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub directory: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: [u8; 32],
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWrite {
    pub process_handle: String,
    #[serde(rename = "deltaBase64", with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalKill {
    pub process_handle: String,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnswer {
    #[serde(with = "crate::protocol::json")]
    pub request_id: Value,
    #[serde(with = "crate::protocol::json")]
    pub result: Value,
}

/// One resumable terminal per device and working directory. Host scopes this ID to the
/// authenticated device, so reopening a view or application can attach safely.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn terminal_handle(cwd: String) -> String {
    format!(
        "bex-terminal-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, cwd.as_bytes())
    )
}

/// Shared xterm palette, also used for Host-owned terminal-query replies.
pub fn terminal_color(index: u16) -> u32 {
    const PALETTE: [u32; 16] = [
        0x181818, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    let index = u32::from(index);
    match index {
        0..=15 => PALETTE[index as usize],
        16..=231 => {
            let n = index - 16;
            let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
            (level(n / 36) << 16) | (level((n / 6) % 6) << 8) | level(n % 6)
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            (v << 16) | (v << 8) | v
        }
        257 => 0x181818,
        _ => 0xe5e5e5,
    }
}
