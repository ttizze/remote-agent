use super::server::{Context, Thread, limit, offset};
use crate::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

pub(crate) fn detail_output() -> String {
    "DEFERRED_DETAIL_FULL_TEXT\n".to_owned() + &"fixture output\n".repeat(500)
}

pub(super) fn persisted(thread: &Thread) -> Option<Thread> {
    let id = thread.metadata["id"].as_str().unwrap();
    let cwd = thread.metadata["cwd"].as_str().unwrap();
    let gallery = thread.metadata.get("gallery") == Some(&Value::Bool(true));
    let long_history = matches!(id, "fixture-long-history" | "fixture-viewport-history");
    if !long_history
        && !cwd.ends_with("large-history")
        && !cwd.ends_with("oversized-history")
        && !gallery
    {
        return None;
    }
    let mut turns = if long_history {
        let sizes = [286, 6, 325, 393, 859, 424, 21, 609, 641, 154];
        let inputs = [4, 1, 6, 8, 1, 2, 1, 4, 1, 2];
        let statuses = [
            "failed",
            "completed",
            "completed",
            "completed",
            "completed",
            "completed",
            "completed",
            "interrupted",
            "completed",
            "interrupted",
        ];
        sizes.iter().zip(inputs).zip(statuses).enumerate().map(|(number, ((size, users), status))| {
            let boundaries: Vec<_> = (0..users).map(|index| index * (size - 1) / users).collect();
            let items: Vec<_> = (0..*size).map(|index| {
                let id = format!("long-{number}-{index}");
                if boundaries.contains(&index) {
                    json!({"id":id,"type":"userMessage","content":[{"type":"text","text":format!("Review section {number}, input {index}.")}]})
                } else if index == size - 1 || index % 5 == 0 {
                    json!({"id":id,"type":"agentMessage","text":format!("Section {number}, progress {index}. ").repeat(4),
                        "phase":if status == "completed" && index == size - 1 { "final_answer" } else { "commentary" }})
                } else {
                    json!({"id":id,"type":"commandExecution","command":format!("inspect section-{number}/file-{index}.txt"),
                        "status":"completed","aggregatedOutput":"Inspection complete.\n".repeat(12),"exitCode":0})
                }
            }).collect();
            Rc::new(RefCell::new(json!({"id":format!("long-turn-{number}"),"status":status,"items":items})))
        }).collect::<Vec<_>>()
    } else if cwd.ends_with("oversized-history") {
        vec![Rc::new(RefCell::new(
            json!({"id":"oversized-turn","status":"completed","items":[
                {"id":"oversized-text","type":"agentMessage","text":"x".repeat(17 * 1024 * 1024)},
                {"id":"oversized-image","type":"imageGeneration","result":"A".repeat(17 * 1024 * 1024)},
                {"id":"oversized-tool","type":"commandExecution","aggregatedOutput":"z".repeat(17 * 1024 * 1024)}
            ]}),
        ))]
    } else if cwd.ends_with("large-history") {
        vec![Rc::new(RefCell::new(
            json!({"id":"large-turn","status":"completed","items":[
            {"id":"large-user","type":"userMessage","content":[{"type":"text","text":"Read the whole output"}]},
            {"id":"large-command","type":"commandExecution","command":"cat output.txt","status":"completed","aggregatedOutput":"output line\n".repeat(700000) + "END_OF_LARGE_OUTPUT"},
            {"id":"large-final","type":"agentMessage","phase":"final_answer","text":"Large history is complete"}]}),
        ))]
    } else {
        thread.turns.clone()
    };
    if long_history {
        if id == "fixture-viewport-history" {
            // Four activity-only turns leave the initial summary shorter than
            // the viewport. Older summary pages must load automatically.
            for turn in &turns[5..9] {
                let item_id = format!("{}-work", turn.borrow()["id"].as_str().unwrap());
                turn.borrow_mut()["items"] = json!([{"id":item_id,
                    "type":"commandExecution","command":"inspect","status":"completed","aggregatedOutput":"done"}]);
            }
        }
        let mut last = turns.last().unwrap().borrow_mut();
        if id == "fixture-viewport-history" {
            last["status"] = "completed".into();
        }
        let item = last["items"].as_array_mut().unwrap().last_mut().unwrap();
        item["id"] = "long-latest-message".into();
        item["text"] = "Latest interrupted conversation message is visible.".into();
        if id == "fixture-viewport-history" {
            item["phase"] = "final_answer".into();
        }
        drop(last);
        // Keep turns started through the fixture when the client reopens history.
        // Only the list's placeholder turn is replaced by the synthetic archive.
        turns.extend(
            thread
                .turns
                .iter()
                .filter(|turn| turn.borrow()["id"] != format!("turn-{id}"))
                .cloned(),
        );
    }
    if gallery && let Some(last) = turns.last_mut() {
        let mut latest = last.borrow().clone();
        latest["items"].as_array_mut().unwrap().extend((0..600).map(|index| json!({"id":format!("gallery-new-command-{index}"),
                "type":"commandExecution","status":"completed","command":"inspect","aggregatedOutput":"done"})));
        *last = Rc::new(RefCell::new(latest));
    }
    Some(Thread {
        metadata: thread.metadata.clone(),
        turns,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    data: Vec<T>,
    next_cursor: Option<String>,
}

pub(super) fn page(
    context: &Context,
    id: &Value,
    thread: &Thread,
    method: &str,
    params: &Value,
) -> Result<()> {
    if let Some(error) = thread.metadata.get("fixtureHistoryError") {
        return context.error(
            id,
            error["code"].as_i64().unwrap().try_into().unwrap(),
            error["message"].as_str().unwrap(),
        );
    }
    let persisted = persisted(thread);
    let thread = persisted.as_ref().unwrap_or(thread);
    if thread.turns.is_empty()
        || thread
            .turns
            .iter()
            .all(|turn| turn.borrow()["items"].as_array().is_some_and(Vec::is_empty))
    {
        return context.error(
            id,
            -32600,
            &format!(
                "thread {} is not materialized yet; thread/turns/list is unavailable before first user message",
                thread.metadata["id"].as_str().unwrap()
            ),
        );
    }
    let turns: Vec<_> = thread.turns.iter().map(|turn| turn.borrow()).collect();
    let offset = offset(params);
    let count = limit(params, 10);
    let descending = params["sortDirection"] == "desc";
    if method == "thread/turns/list" {
        context.trace(method, params.clone())?;
        if let Some(response) = thread.metadata.get("fixtureHistoryResponse") {
            return context.respond(id, response);
        }
        let mut values: Vec<Value> = turns
            .iter()
            .map(|turn| {
                let mut turn = (**turn).clone();
                let items = turn["items"].as_array().unwrap();
                let view = params["itemsView"].as_str().unwrap_or("summary");
                turn["items"] = match view {
                    "notLoaded" => json!([]),
                    "summary" => {
                        let mut summary = Vec::new();
                        if let Some(item) = items.iter().find(|item| item["type"] == "userMessage")
                        {
                            summary.push(item.clone());
                        }
                        if let Some(item) = items.iter().rfind(|item| {
                            item["type"] == "agentMessage"
                                && (item["phase"].is_null() || item["phase"] == "final_answer")
                        }) {
                            summary.push(item.clone());
                        }
                        json!(summary)
                    }
                    _ => json!(items),
                };
                turn["itemsView"] = view.into();
                turn
            })
            .collect();
        if descending {
            values.reverse();
        }
        let total = values.len();
        let data = values.into_iter().skip(offset).take(count).collect();
        context.respond(
            id,
            &Page {
                data,
                next_cursor: (offset.saturating_add(count) < total)
                    .then(|| offset.saturating_add(count).to_string()),
            },
        )
    } else {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Item<'a> {
            turn_id: &'a Value,
            item: &'a Value,
        }
        context.trace(method, params.clone())?;
        let mut values: Vec<_> = turns
            .iter()
            .filter(|turn| turn["id"] == params["turnId"])
            .flat_map(|turn| {
                turn["items"].as_array().unwrap().iter().map(|item| Item {
                    turn_id: &turn["id"],
                    item,
                })
            })
            .collect();
        if descending {
            values.reverse();
        }
        let total = values.len();
        let end = offset.saturating_add(count);
        let data = values.into_iter().skip(offset).take(count).collect();
        context.respond(
            id,
            &Page {
                data,
                next_cursor: (end < total).then(|| end.to_string()),
            },
        )
    }
}
