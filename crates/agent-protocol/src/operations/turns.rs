//! Turn, thread, and submission records. Input JSON tagging is owned here.
use super::RpcMethod;
use crate::error::PeerError;
use crate::models::ThreadResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

pub fn validate_thread(
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
    crate::operations::rpc_contract!(Transcribe);
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(Transcribe {
            audio: self.audio.as_ref().to_vec(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    #[serde(with = "crate::protocol::json")]
    pub request_id: Value,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnswer {
    #[serde(with = "crate::protocol::json")]
    pub request_id: Value,
    #[serde(with = "crate::protocol::json")]
    pub result: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkThread {
    pub thread_id: String,
    pub last_turn_id: String,

    #[serde(default)]
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
    pub(crate) fn validate(
        &self,
        output: &crate::models::ThreadResponse,
    ) -> Result<(), &'static str> {
        validate_thread(output, None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartThread {
    pub cwd: Option<String>,
    pub model: Option<String>,
}

impl StartThread {
    pub(crate) fn validate(
        &self,
        output: &crate::models::ThreadResponse,
    ) -> Result<(), &'static str> {
        validate_thread(output, None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interrupt {
    pub thread_id: String,
    pub turn_id: String,
}
impl ReadItem {
    pub(crate) fn validate(&self, output: &ItemResponse) -> Result<(), &'static str> {
        if output.item.id == self.item_id.as_str() {
            Ok(())
        } else {
            Err("item ID does not match")
        }
    }
}

/// Input intent. The Host chooses start, steer or queue from its current execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub thread_id: String,
    pub client_user_message_id: String,
    #[serde(with = "input_json")]
    pub input: Vec<Input>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(rename = "serviceTierForTurn")]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionReceipt {
    pub turn_id: Option<String>,
}
