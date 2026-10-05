//! Native orchestration RPC parameters. Shared domain records live in orchestration.
use ::orchestration::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchReceipt {
    pub thread_id: ThreadId,
    pub sequence: u64,
    pub replayed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchThread {
    pub create: Command,
    pub input: MessageDispatch,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeShell {
    pub after_sequence: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeThread {
    pub thread_id: ThreadId,
    pub after_sequence: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetThreadProjection {
    pub thread_id: ThreadId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTurnItem {
    pub thread_id: ThreadId,
    pub item_id: TurnItemId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadThreadHistory {
    pub thread_id: ThreadId,
    pub cursor: Option<HistoryCursor>,
    pub limit: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchThreads {
    pub query: String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTurnDiff {
    pub thread_id: ThreadId,
    pub from_turn_count: u64,
    pub to_turn_count: u64,
    pub ignore_whitespace: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiff {
    pub thread_id: ThreadId,
    pub from_turn_count: u64,
    pub to_turn_count: u64,
    pub diff: String,
}
