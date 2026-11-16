//! Revert before a stable native turn ID. On retry, a retained head proves an
//! already-applied write; a missing boundary never means "remove N more turns".
use super::{Failure, Page};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, future::Future};

#[derive(Deserialize)]
struct Turn {
    id: String,
}

fn invalid(message: &str) -> Failure {
    Failure::new("invalid_rollback_history", message)
}

pub(super) async fn request<F, Fut>(
    thread: &str,
    retained: Option<&str>,
    removed: &str,
    mut call: F,
) -> Result<(), Failure>
where
    F: FnMut(&'static str, Value) -> Fut,
    Fut: Future<Output = Result<Value, Failure>>,
{
    let mut cursor: Option<String> = None;
    let mut visited = BTreeSet::new();
    let mut turns = Vec::new();
    let mut identities = BTreeSet::new();
    loop {
        if !visited.insert(cursor.clone()) {
            return Err(invalid("Thread history pagination repeated a cursor"));
        }
        let page: Page<Turn> = serde_json::from_value(call("thread/turns/list", json!({
            "threadId":thread,"cursor":cursor,"limit":100,"sortDirection":"desc","itemsView":"summary"
        })).await?)?;
        if page.data.len() > 100
            || page
                .data
                .iter()
                .any(|turn| turn.id.is_empty() || !identities.insert(turn.id.clone()))
        {
            return Err(invalid(
                "Thread history contains invalid or repeated turn identities",
            ));
        }
        turns.extend(page.data.into_iter().map(|turn| turn.id));
        if retained.is_some_and(|id| turns.iter().any(|turn| turn == id))
            || (retained.is_none() && turns.iter().any(|turn| turn == removed))
        {
            break;
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let boundary = turns.iter().position(|turn| turn == removed);
    if boundary.is_none() {
        return if turns.first().map(String::as_str) == retained {
            Ok(())
        } else {
            Err(invalid(
                "The rollback boundary is unavailable; no native history was changed",
            ))
        };
    }
    if let Some(retained) = retained
        && turns
            .iter()
            .position(|turn| turn == retained)
            .is_none_or(|index| index <= boundary.unwrap())
    {
        return Err(invalid(
            "The retained native turn is not before the rollback boundary",
        ));
    }
    let response = call(
        "thread/revert",
        json!({"threadId":thread,"beforeTurnId":removed}),
    )
    .await?;
    if response["thread"]["id"].as_str() != Some(thread) {
        return Err(invalid(
            "Codex returned a different native thread after rollback",
        ));
    }
    Ok(())
}
