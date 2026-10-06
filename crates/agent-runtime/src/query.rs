//! Reads outside the subscription streams: history pages, one turn item, and search.
use crate::sync::{
    HistoryPage, HistoryRow, InvalidCursor, PagePolicy, ProjectDirectory, client_state,
    detail_item, history_before, js_space, recent_history, timeline,
};
use crate::{ActorHandle, RuntimeError, Store, StoreError};
use agent_domain::{State, ThreadId, Timestamp, TurnItemId};
use rusqlite::params;
use serde::{Deserialize, Serialize};

pub const SEARCH_MIN_QUERY_CHARS: usize = 2;
pub const SEARCH_MAX_QUERY_CHARS: usize = 200;
pub const SEARCH_MAX_LIMIT: usize = 50;
pub const SEARCH_SNIPPET_CHARS: usize = 240;

#[derive(Debug, Clone, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Cursor(#[from] InvalidCursor),
    #[error("search needs 2 to 200 characters and a limit of 1 to 50")]
    InvalidSearch,
    /// Carries no query text: search input is user content.
    #[error("thread search {operation} failed")]
    Search { operation: &'static str },
}

impl ActorHandle {
    /// The page before `cursor`, or the newest page without one.
    pub async fn history(&self, cursor: Option<&str>) -> Result<HistoryPage, QueryError> {
        let view = self.view().await?;
        let state = client_state(&view.state);
        Ok(match cursor {
            Some(cursor) => history_before(&state, cursor, view.head.global_seq, None)?,
            None => recent_history(&state, view.head.global_seq, PagePolicy::RECENT),
        })
    }

    pub async fn turn_item(&self, item: &TurnItemId) -> Result<Option<HistoryRow>, QueryError> {
        Ok(turn_item(&self.view().await?.state, item))
    }
}

/// One visible item with its message and plan, at its timeline position, with the
/// detail the timeline withholds.
pub fn turn_item(state: &State, item: &TurnItemId) -> Option<HistoryRow> {
    timeline(state)
        .iter()
        .enumerate()
        .find(|(_, row)| &row.item.id == item)
        .map(|(position, row)| {
            let mut owned = row.owned(position);
            owned.item = detail_item(row.item);
            owned
        })
}

/// Every visible item in timeline order, positioned as history pages position them.
pub fn timeline_rows(state: &State) -> Vec<HistoryRow> {
    timeline(state)
        .iter()
        .enumerate()
        .map(|(position, row)| row.owned(position))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub thread: ThreadId,
    pub project: String,
    pub source: SearchSource,
    pub snippet: String,
    /// A message may lack a creation time.
    pub message_created_at: Option<Timestamp>,
}

fn like_pattern(query: &str) -> String {
    let escaped = query
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_");
    format!("%{escaped}%")
}

/// JavaScript `text.replace(/\s+/g, " ").trim()`.
fn collapse_space(text: &str) -> String {
    text.split(js_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// At most 240 UTF-16 units, centred near the first match.
pub fn search_snippet(text: &str, query: &str) -> String {
    let normalized = collapse_space(text);
    let units: Vec<u16> = normalized.encode_utf16().collect();
    if units.len() <= SEARCH_SNIPPET_CHARS {
        return normalized;
    }
    let fold = |unit: u16| match unit {
        0x41..=0x5A => unit + 0x20,
        unit => unit,
    };
    let needle: Vec<u16> = collapse_space(query).encode_utf16().map(fold).collect();
    let folded: Vec<u16> = units.iter().copied().map(fold).collect();
    let found = if needle.is_empty() {
        Some(0)
    } else {
        folded
            .windows(needle.len())
            .position(|window| window == needle)
    };
    let body = SEARCH_SNIPPET_CHARS - 4;
    let ideal = found.map_or(0, |index| index.saturating_sub(72));
    let start = ideal.min(units.len() - body);
    let end = units.len().min(start + body);
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        String::from_utf16_lossy(&units[start..end]),
        if end < units.len() { "…" } else { "" }
    )
}

type SearchRowData = (String, String, String, String, Option<String>);

/// The best match per thread, ordered and limited in SQL: user messages outrank
/// assistant ones, then the newest wins; threads order by match kind, then the
/// thread's activity time, which visits do not change.
pub(crate) fn search_rows(
    c: &rusqlite::Connection,
    pattern: &str,
    projects: &[String],
    limit: usize,
) -> Result<Vec<SearchRowData>, StoreError> {
    let mut statement = c.prepare_cached(
        "WITH candidate AS (
             SELECT m.thread_id, s.project, m.role, m.text, m.created_at, m.message_id,
                    json_extract(s.payload, '$.updated_at') AS thread_updated_at
             FROM search_messages AS m
             JOIN thread_shells AS s ON s.thread_id = m.thread_id
             WHERE s.deleted = 0 AND s.archived = 0
               AND s.project IN (SELECT value FROM json_each(?2))
               AND m.role IN ('user', 'assistant')
               AND m.text LIKE ?1 ESCAPE '!'
         ),
         ranked AS (
             SELECT *, CASE role WHEN 'user' THEN 0 ELSE 1 END AS match_rank,
                    ROW_NUMBER() OVER (
                        PARTITION BY thread_id
                        ORDER BY CASE role WHEN 'user' THEN 0 ELSE 1 END,
                                 created_at DESC, message_id
                    ) AS thread_rank
             FROM candidate
         )
         SELECT thread_id, project, role, text, created_at FROM ranked
         WHERE thread_rank = 1
         ORDER BY match_rank, thread_updated_at DESC, thread_id
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![pattern, serde_json::to_string(projects)?, limit as i64],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        },
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

impl Store {
    /// Finished user and assistant messages of live threads in live projects, one
    /// match per thread.
    pub fn search(
        &self,
        query: &str,
        limit: Option<usize>,
        projects: &dyn ProjectDirectory,
    ) -> Result<Vec<SearchMatch>, QueryError> {
        let query = query.trim_matches(js_space);
        let limit = limit.unwrap_or(SEARCH_MAX_LIMIT);
        if !(SEARCH_MIN_QUERY_CHARS..=SEARCH_MAX_QUERY_CHARS)
            .contains(&query.encode_utf16().count())
            || !(1..=SEARCH_MAX_LIMIT).contains(&limit)
        {
            return Err(QueryError::InvalidSearch);
        }
        let live: Vec<String> = projects
            .projects()
            .into_iter()
            .map(|project| project.id)
            .collect();
        let rows = self
            .read(|c| search_rows(c, &like_pattern(query), &live, limit))
            .map_err(|_: StoreError| QueryError::Search { operation: "query" })?;
        let decode = QueryError::Search {
            operation: "decode",
        };
        rows.into_iter()
            .map(|(thread, project, role, text, created_at)| {
                Ok(SearchMatch {
                    thread: ThreadId::new(thread).map_err(|_| decode.clone())?,
                    project,
                    source: match role.as_str() {
                        "user" => SearchSource::User,
                        _ => SearchSource::Assistant,
                    },
                    snippet: search_snippet(&text, query),
                    message_created_at: created_at
                        .map(|created_at| Timestamp::parse(&created_at))
                        .transpose()
                        .map_err(|_| decode.clone())?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
