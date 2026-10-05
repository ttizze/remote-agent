//! Reads outside the subscription streams: history pages, one turn item, and search.
use crate::sync::{
    HistoryPage, HistoryRow, InvalidCursor, PagePolicy, ProjectDirectory, history_before,
    recent_history, timeline,
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
        Ok(match cursor {
            Some(cursor) => history_before(&view.state, cursor, view.head.global_seq, None)?,
            None => recent_history(&view.state, view.head.global_seq, PagePolicy::RECENT),
        })
    }

    pub async fn turn_item(&self, item: &TurnItemId) -> Result<Option<HistoryRow>, QueryError> {
        Ok(turn_item(&self.view().await?.state, item))
    }
}

/// One visible item with its message and plan, at its timeline position.
pub fn turn_item(state: &State, item: &TurnItemId) -> Option<HistoryRow> {
    timeline(state)
        .iter()
        .enumerate()
        .find(|(_, row)| &row.item.id == item)
        .map(|(position, row)| row.owned(position))
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
    pub message_created_at: Timestamp,
}

fn like_pattern(query: &str) -> String {
    let escaped = query
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_");
    format!("%{escaped}%")
}

/// At most 240 characters, centred near the first match.
pub fn search_snippet(text: &str, query: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= SEARCH_SNIPPET_CHARS {
        return normalized;
    }
    let needle: Vec<char> = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
        .chars()
        .collect();
    let folded: Vec<char> = chars.iter().map(char::to_ascii_lowercase).collect();
    let found = (!needle.is_empty())
        .then(|| {
            folded
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten();
    let body = SEARCH_SNIPPET_CHARS - 4;
    let ideal = found.map_or(0, |index| index.saturating_sub(72));
    let start = ideal.min(chars.len() - body);
    let end = chars.len().min(start + body);
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        chars[start..end].iter().collect::<String>(),
        if end < chars.len() { "…" } else { "" }
    )
}

impl Store {
    /// Finished user and assistant messages of live threads in live projects. One
    /// match per thread: user messages outrank assistant ones, then the newest wins.
    /// Threads order by match kind, then recent activity.
    pub fn search(
        &self,
        query: &str,
        limit: Option<usize>,
        projects: &dyn ProjectDirectory,
    ) -> Result<Vec<SearchMatch>, QueryError> {
        let query = query.trim();
        let limit = limit.unwrap_or(SEARCH_MAX_LIMIT);
        if !(SEARCH_MIN_QUERY_CHARS..=SEARCH_MAX_QUERY_CHARS).contains(&query.chars().count())
            || !(1..=SEARCH_MAX_LIMIT).contains(&limit)
        {
            return Err(QueryError::InvalidSearch);
        }
        let rows = self
            .read(|c| {
                let mut statement = c.prepare_cached(
                    "WITH candidate AS (
                         SELECT m.thread_id, s.project, m.role, m.text, m.created_at,
                                m.message_id, t.last_global_seq
                         FROM search_messages AS m
                         JOIN thread_shells AS s ON s.thread_id = m.thread_id
                         JOIN threads AS t ON t.thread_id = m.thread_id
                         WHERE s.deleted = 0 AND s.archived = 0
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
                     ORDER BY match_rank, last_global_seq DESC, thread_id",
                )?;
                let rows = statement.query_map(params![like_pattern(query)], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .map_err(|_: StoreError| QueryError::Search { operation: "query" })?;
        let decode = QueryError::Search {
            operation: "decode",
        };
        let mut matches = Vec::new();
        for (thread, project, role, text, created_at) in rows {
            if matches.len() == limit {
                break;
            }
            if projects.project(&project).is_none() {
                continue;
            }
            matches.push(SearchMatch {
                thread: ThreadId::new(thread).map_err(|_| decode.clone())?,
                project,
                source: match role.as_str() {
                    "user" => SearchSource::User,
                    _ => SearchSource::Assistant,
                },
                snippet: search_snippet(&text, query),
                message_created_at: Timestamp::parse(&created_at).map_err(|_| decode.clone())?,
            });
        }
        Ok(matches)
    }
}

#[cfg(test)]
mod tests;
