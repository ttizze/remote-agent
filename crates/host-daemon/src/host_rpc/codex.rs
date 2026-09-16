//! Codex native protocol boundary: one shared process, ordered request
//! completion, native cursors, deferred item hydration and detail reads.
use super::service::Failure;
use crate::desktop_projects::ThreadPage;
use agent_core::{
    models::{Item, Thread, ThreadResponse, Turn},
    peer::RpcResponse,
    state::operations as op,
};
use codex_app_server::CodexAppServer;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadListParams<'a> {
    pub limit: usize,
    pub sort_key: &'a str,
    pub sort_direction: &'a str,
    pub use_state_db_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_term: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

use std::sync::Arc;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryParams<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
    pub limit: usize,
    pub sort_direction: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items_view: Option<&'a str>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryPage<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryItem {
    pub item: Arc<Item>,
    pub turn_id: Option<String>,
}
impl HistoryPage<HistoryItem> {
    fn into_items(self) -> Result<(Vec<Arc<Item>>, Option<String>), &'static str> {
        if self.data.len() > 100 {
            return Err("item page exceeds requested size");
        }
        let items = self
            .data
            .into_iter()
            .map(|entry| {
                if entry.item.id.is_empty() {
                    Err("history item ID is missing")
                } else {
                    Ok(entry.item)
                }
            })
            .collect::<Result<_, _>>()?;
        Ok((items, self.next_cursor))
    }
}

pub(super) struct Codex {
    pub(super) process: Result<Arc<CodexAppServer>, String>,
    pub(super) stopped: tokio_util::sync::CancellationToken,
    pub(super) processed: tokio::sync::watch::Sender<u64>,
}
impl Codex {
    pub(super) fn new(process: Result<Arc<CodexAppServer>, String>) -> Self {
        Self {
            process,
            stopped: Default::default(),
            processed: tokio::sync::watch::channel(0).0,
        }
    }
    pub(super) fn server(&self) -> Result<&CodexAppServer, Failure> {
        if self.stopped.is_cancelled() {
            return Err(Failure::new(
                "codex_unavailable",
                "Codexが終了しました。Hostを再起動すると再接続できます。Claudeの会話は継続できます。",
            ));
        }
        self.process
            .as_deref()
            .map_err(|error| Failure::new("codex_unavailable", error))
    }

    pub(super) async fn request(&self, line: &str) -> Result<String, Failure> {
        let reply = self
            .server()?
            .request_raw_sequenced(line)
            .await
            .map_err(Failure::from)?;
        let mut processed = self.processed.subscribe();
        tokio::select! {
            result = processed.wait_for(|sequence| *sequence >= reply.sequence) => {
                result.map_err(|_| Failure::new("codex_unavailable", "Codex event pump stopped"))?;
            }
            _ = self.stopped.cancelled() => return Err(Failure::new("codex_unavailable", "Codex event stream is unavailable")),
        }
        Ok(reply.value)
    }

    pub(super) async fn thread_page(
        &self,
        params: &ThreadListParams<'_>,
    ) -> Result<ThreadPage, Failure> {
        self.server()?
            .request("thread/list", params)
            .await
            .map_err(Failure::from)?
            .outcome
            .map_err(Failure::Upstream)
    }

    pub(super) async fn history(
        &self,
        id: &str,
        paginated: bool,
        defer: bool,
        limit: usize,
    ) -> Result<Thread, Failure> {
        if !paginated {
            let line = self.request(&serde_json::json!({"id":0,"method":"thread/read","params":{"threadId":id,"includeTurns":true}}).to_string()).await?;
            let mut thread = RpcResponse::<ThreadResponse>::parse(&line)?
                .outcome
                .map_err(Failure::Upstream)?
                .thread;
            if thread.id.as_deref() != Some(id) {
                return Err(Failure::new(
                    "invalid_thread_history",
                    "native session identity changed",
                ));
            }
            if let Some(turns) = &mut thread.turns
                && turns.len() > limit
            {
                turns.drain(..turns.len() - limit);
                thread.extra.insert("historyHasMore".into(), true.into());
            }
            return Ok(thread);
        }
        let query = HistoryParams {
            thread_id: id,
            turn_id: None,
            cursor: None,
            limit,
            sort_direction: "desc",
            items_view: Some(if defer { "notLoaded" } else { "full" }),
        };
        let mut page = match self
            .history_request::<Arc<Turn>>("thread/turns/list", &query)
            .await
        {
            Ok(page) => page,
            Err(error)
                if serde_json::from_str::<serde_json::Value>(&error)
                    .ok()
                    .is_some_and(|value| value["code"] == -32601) =>
            {
                // Native servers advertise pagination before materializing a
                // first turn. Read that same native session without cursors.
                return match Box::pin(self.history(id, false, defer, limit)).await {
                    Ok(history) => Ok(history),
                    Err(error) => Ok(Thread {
                        id: Some(id.into()),
                        extra: [(
                            "historyReadState".into(),
                            serde_json::json!({"type":"unavailable","issues":[error.to_string()]}),
                        )]
                        .into_iter()
                        .collect(),
                        ..Default::default()
                    }),
                };
            }
            Err(error) => return Err(Failure::new("invalid_thread_history", error)),
        };
        if defer {
            self.hydrate_turn_page(&mut page, &query)
                .await
                .map_err(|error| Failure::new("invalid_thread_history", error))?;
        }
        let mut thread = Thread {
            id: Some(id.into()),
            ..Default::default()
        };
        page.data.reverse();
        thread.turns = Some(page.data);
        thread.extra.insert(
            "historyHasMore".into(),
            page.next_cursor
                .is_some_and(|cursor| !cursor.is_empty())
                .into(),
        );
        Ok(thread)
    }
    // Keep App Server cursors opaque. Both initial hydration and older pages
    // use the desktop five-turn / 500-item initial window, in pages of 100.
    async fn history_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &HistoryParams<'_>,
    ) -> Result<HistoryPage<T>, String> {
        let line = serde_json::json!({"id":0,"method":method,"params":params}).to_string();
        let response = self
            .request(&line)
            .await
            .map_err(|error| error.to_string())?;
        RpcResponse::<HistoryPage<T>>::parse(&response)
            .map_err(|error| error.to_string())?
            .outcome
            .map_err(|error| error.get().to_owned())
    }

    async fn hydrate_turn_page(
        &self,
        page: &mut HistoryPage<Arc<Turn>>,
        query: &HistoryParams<'_>,
    ) -> Result<(), String> {
        if page.data.len() > query.limit {
            return Err("turn page exceeds requested size".into());
        }
        let thread_id = query.thread_id;
        let mut ids = std::collections::HashSet::new();
        let repeated = page.data.iter().any(|turn| !ids.insert(turn.id.as_str()));
        if repeated {
            // Item pagination is keyed only by turn ID, so it cannot preserve
            // boundaries between historical occurrences sharing that ID.
            let full = self
                .history_request::<Arc<Turn>>(
                    "thread/turns/list",
                    &HistoryParams {
                        items_view: Some("full"),
                        ..*query
                    },
                )
                .await?;
            if full.next_cursor != page.next_cursor
                || !full
                    .data
                    .iter()
                    .map(|turn| &turn.id)
                    .eq(page.data.iter().map(|turn| &turn.id))
            {
                return Err("turn history changed while loading repeated IDs".into());
            }
            if full
                .data
                .iter()
                .any(|turn| turn.items.is_none() || turn.items_view.as_deref() == Some("notLoaded"))
            {
                return Err("full turn history omitted repeated-turn items".into());
            }
            *page = full;
            return Ok(());
        }
        let mut budget = query.limit.saturating_mul(100).min(100_000);
        for turn in &mut page.data {
            let turn = Arc::make_mut(turn);
            let mut values = Vec::new();
            let mut cursor = None;
            let mut has_more = true;
            let mut cursors = std::collections::HashSet::new();
            let mut ids = std::collections::HashSet::new();
            while has_more && budget > 0 {
                let page = self
                    .history_request::<HistoryItem>(
                        "thread/items/list",
                        &HistoryParams {
                            thread_id,
                            turn_id: Some(&turn.id),
                            cursor: cursor.as_deref(),
                            limit: budget.min(100),
                            sort_direction: "desc",
                            items_view: None,
                        },
                    )
                    .await?;
                let (items, next_cursor) = page.into_items()?;
                cursor = next_cursor;
                for item in items {
                    if ids.insert(item.id.clone()) {
                        budget = budget
                            .checked_sub(1)
                            .ok_or("item page exceeds requested budget")?;
                        values.push(item);
                    }
                }
                has_more = cursor.is_some();
                if let Some(cursor) = &cursor
                    && !cursors.insert(cursor.clone())
                {
                    return Err("history cursor repeated".into());
                }
            }
            values.reverse();
            let view = if has_more && values.is_empty() {
                "notLoaded"
            } else if has_more {
                "summary"
            } else {
                "full"
            };
            turn.items = Some(values);
            turn.items_has_more = Some(has_more);
            turn.items_view = Some(view.into());
            self.preserve_opening_question(turn, thread_id).await?;
        }
        Ok(())
    }

    async fn preserve_opening_question(
        &self,
        turn: &mut Turn,
        thread_id: &str,
    ) -> Result<(), String> {
        let values = turn.items.as_deref().ok_or("turn items are missing")?;
        if turn.items_has_more != Some(true) || values.is_empty() {
            return Ok(());
        }
        let opening = self
            .history_request::<HistoryItem>(
                "thread/items/list",
                &HistoryParams {
                    thread_id,
                    turn_id: Some(&turn.id),
                    cursor: None,
                    limit: 2,
                    sort_direction: "asc",
                    items_view: None,
                },
            )
            .await?;
        if let Some(item) = opening
            .into_items()?
            .0
            .into_iter()
            .find(|item| item.kind.as_deref() != Some("contextCompaction"))
            .filter(|item| item.kind.as_deref() == Some("userMessage"))
            && !values.iter().any(|value| value.id == item.id)
        {
            turn.opening_user_message = Some(item);
        }
        Ok(())
    }

    pub(super) async fn item_read(
        &self,
        params: op::ReadItem,
    ) -> Result<agent_core::client::ItemResponse, Failure> {
        if [&params.thread_id, &params.turn_id, &params.item_id]
            .iter()
            .any(|id| id.is_empty())
        {
            return Err(Failure::new(
                "invalid_params",
                "threadId, turnId and itemId are required",
            ));
        }
        let mut cursor = None;
        let mut cursors = std::collections::HashSet::new();
        loop {
            let query = HistoryParams {
                thread_id: &params.thread_id,
                turn_id: Some(&params.turn_id),
                limit: 100,
                sort_direction: "asc",
                cursor: cursor.as_deref(),
                items_view: None,
            };
            let response = self
                .server()?
                .request::<_, HistoryPage<HistoryItem>>("thread/items/list", &query)
                .await
                .map_err(Failure::from)?;
            let page = response.outcome.map_err(Failure::Upstream)?;
            if let Some(entry) = page.data.into_iter().find(|entry| {
                entry.turn_id.as_deref() == Some(params.turn_id.as_str())
                    && entry.item.id == params.item_id
            }) {
                return Ok(agent_core::client::ItemResponse {
                    item: Arc::unwrap_or_clone(entry.item),
                    extra: Default::default(),
                });
            }
            cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
            let Some(next) = &cursor else {
                return Err(Failure::new(
                    "item_not_found",
                    "The activity is no longer available. Refresh the task.",
                ));
            };
            if !cursors.insert(next.clone()) {
                return Err(Failure::new(
                    "invalid_thread_history",
                    "item page cursor repeated",
                ));
            }
        }
    }
}
