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
    let history_kind = thread
        .metadata
        .get("fixtureHistory")
        .and_then(Value::as_str);
    let long_history = matches!(
        history_kind,
        Some("long" | "viewport" | "render" | "running")
    );
    let viewport = history_kind == Some("viewport");
    // The long history with its latest turn still working, as a conversation
    // reopened while its agent runs.
    let running = history_kind == Some("running");
    let reading = history_kind == Some("reading");
    if !long_history
        && !reading
        && !cwd.ends_with("large-history")
        && !cwd.ends_with("oversized-history")
        && !gallery
    {
        return None;
    }
    let mut turns = if reading {
        (0..READING_TURNS).map(reading_turn).collect()
    } else if history_kind == Some("render") {
        render_history()
    } else if long_history {
        // More turns than one ten-turn window, so older history pages remain.
        let sizes = [
            286, 6, 325, 393, 859, 424, 21, 609, 641, 154, 212, 37, 488, 95, 154,
        ];
        let inputs = [4, 1, 6, 8, 1, 2, 1, 4, 1, 2, 3, 1, 5, 1, 2];
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
            "completed",
            "completed",
            "interrupted",
            "completed",
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
        if viewport {
            // The bounded item page starts inside collapsed work, leaving a
            // short viewport with older pages available behind the manual control.
            for turn in &turns[..14] {
                let mut turn = turn.borrow_mut();
                turn["items"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|item| item["type"] == "commandExecution");
            }
        }
        let mut last = turns.last().unwrap().borrow_mut();
        if viewport {
            last["status"] = "completed".into();
            let items = last["items"].as_array_mut().unwrap();
            let answer = items.pop().unwrap();
            items.truncate(1);
            items.push(answer);
        }
        if running {
            last["status"] = "inProgress".into();
        }
        let item = last["items"].as_array_mut().unwrap().last_mut().unwrap();
        item["id"] = "long-latest-message".into();
        item["text"] = "Latest interrupted conversation message is visible.".into();
        if viewport {
            item["phase"] = "final_answer".into();
        }
        drop(last);
    }
    if long_history || reading {
        // Codex item keys are globally strong, including across fixture archives.
        for turn in &turns {
            let mut turn = turn.borrow_mut();
            turn["id"] = format!("{id}:{}", turn["id"].as_str().unwrap()).into();
            for item in turn["items"].as_array_mut().unwrap() {
                item["id"] = format!("{id}:{}", item["id"].as_str().unwrap()).into();
            }
        }
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

/// Turns of the reading history: several ten-turn windows, so older pages remain.
const READING_TURNS: usize = 36;

/// A turn as a long working session leaves it: a question, folded tool work and
/// commentary, and a long Markdown answer with code, a table and lists. Lengths
/// vary by turn so rows differ in height as real answers do.
fn reading_turn(number: usize) -> super::server::Turn {
    let id = |item: &str| format!("reading-{number}-{item}");
    let mut items = vec![
        json!({"id":id("user"),"type":"userMessage","content":[{"type":"text",
            "text":format!("Reading question {number}: how does `module-{number}` retry failed requests, and what should change?")}]}),
        json!({"id":id("reasoning"),"type":"reasoning","summary":["Tracing the retry path"]}),
    ];
    for (index, tool) in ["rg -n retry", "sed -n 1,80p", "cargo test -p"]
        .iter()
        .enumerate()
    {
        items.push(json!({"id":id(&format!("command-{index}")),"type":"commandExecution",
            "command":format!("{tool} crates/module-{number}"),"status":"completed","exitCode":0,
            "aggregatedOutput":(0..24).map(|line| format!("crates/module-{number}/src/retry.rs:{line}: let delay = backoff(attempt);\n")).collect::<String>()}));
    }
    items.push(json!({"id":id("commentary"),"type":"agentMessage","phase":"commentary",
        "text":format!("Module {number}'s retries go through `backoff`; checking its tests next.")}));
    items.push(json!({"id":id("answer"),"type":"agentMessage","phase":"final_answer","text":reading_answer(number)}));
    Rc::new(RefCell::new(
        json!({"id":format!("reading-turn-{number}"),"status":"completed","items":items}),
    ))
}

fn reading_answer(number: usize) -> String {
    let paragraph = format!(
        "Module {number} retries **failed requests** through `backoff(attempt)`, which doubles the delay from 100 ms and stops after the fifth attempt. \
         A request that fails with a *permanent* error is not retried, and the caller sees the first error rather than the last. \
         The [retry policy](crates/module-{number}/src/retry.rs) keeps its state per request, so concurrent requests never share a delay.\n\n"
    );
    let code: String = (0..10 + number * 7 % 20)
        .map(|line| {
            format!(
                "    let delay_{line} = backoff(attempt + {line}).min(Duration::from_secs(30));\n"
            )
        })
        .collect();
    let rows: String = (1..=5)
        .map(|attempt| format!("| {attempt} | {} ms | retried |\n", 100 << (attempt - 1)))
        .collect();
    format!(
        "## Module {number} retries\n\n{paragraphs}\
         - The first attempt runs at once.\n- Later attempts wait for `backoff`.\n- A permanent error stops at once.\n\
         - Each request keeps its own delay.\n- The limit is five attempts.\n- Cancellation stops the wait.\n\n\
         ```rust title=crates/module-{number}/src/retry.rs\npub fn retry(attempt: u32) -> Duration {{\n{code}    backoff(attempt)\n}}\n```\n\n\
         | Attempt | Delay | Outcome |\n|---|---:|---|\n{rows}\n\
         1. Keep the five-attempt limit.\n2. Report the first permanent error.\n3. Add a test for cancellation.\n\n\
         > The policy matches the other modules.\n\nAnswer {number} ends here.",
        paragraphs = paragraph.repeat(1 + number % 3),
    )
}

/// Many complete exchanges with prose, code and tools, spanning several Host
/// pages. Each answer has identifiable paragraphs for native paint assertions.
fn render_history() -> Vec<Rc<RefCell<Value>>> {
    (0..60).map(|turn| {
        let prose = (0..8).map(|paragraph| format!(
            "Render turn {turn}, paragraph {paragraph}. **The reader keeps their place** while older history loads. We inspect the request, compare the response and retain the measured result. A [reference](https://example.com) and `request_id` stay selectable.\n\n"
        )).collect::<String>();
        let answer = format!("## Rendering review {turn}\n\n{prose}```rust\nfn apply_response(text: &str) -> String {{\n    format!(\"response: {{text}}\")\n}}\n```\n\nRender answer {turn} complete.");
        Rc::new(RefCell::new(json!({"id":format!("render-turn-{turn}"),"status":"completed","items":[
            {"id":format!("render-user-{turn}"),"type":"userMessage","content":[{"type":"text","text":format!("Review rendering turn {turn}.")}]},
            {"id":format!("render-tool-{turn}"),"type":"commandExecution","command":"cargo nextest run -p renderer","status":"completed","aggregatedOutput":"running 24 tests\n24 passed","exitCode":0},
            {"id":format!("render-answer-{turn}"),"type":"agentMessage","phase":"final_answer","text":answer}
        ]})))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_history_pages_long_answers_and_keeps_the_scenarios_live_turns() {
        let live = Rc::new(RefCell::new(
            json!({"id":"live-turn", "status":"completed", "items":[]}),
        ));
        let thread = Thread {
            metadata: serde_json::from_value(
                json!({"id":"reading", "cwd":"/fixture", "fixtureHistory":"reading"}),
            )
            .unwrap(),
            turns: vec![live],
        };
        let history = persisted(&thread).unwrap();
        assert_eq!(history.turns.len(), READING_TURNS + 1);
        assert_eq!(history.turns.last().unwrap().borrow()["id"], "live-turn");
        let last = history.turns[READING_TURNS - 1].borrow();
        let answer = last["items"].as_array().unwrap().last().unwrap();
        assert_eq!(answer["phase"], "final_answer");
        let text = answer["text"].as_str().unwrap();
        assert!(text.contains("```rust") && text.contains("| Attempt |"));
        assert!(text.ends_with(&format!("Answer {} ends here.", READING_TURNS - 1)));
    }

    #[test]
    fn synthetic_item_ids_are_stable_and_distinct_between_conversations() {
        let ids = |id: &str| {
            let thread = Thread {
                metadata: serde_json::from_value(
                    json!({"id":id,"cwd":"/fixture","fixtureHistory":"reading"}),
                )
                .unwrap(),
                turns: vec![],
            };
            persisted(&thread)
                .unwrap()
                .turns
                .iter()
                .flat_map(|turn| {
                    turn.borrow()["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item["id"].as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        let first = ids("first");
        assert!(!first.is_empty());
        assert_eq!(first, ids("first"));
        assert!(first.is_disjoint(&ids("second")));
    }

    #[test]
    fn history_kind_is_explicit_and_reopening_retains_the_scenarios_live_turns() {
        for (kind, status, phase) in [
            ("long", "interrupted", "commentary"),
            ("viewport", "completed", "final_answer"),
        ] {
            let live = Rc::new(RefCell::new(json!({
                "id":"live-turn", "status":"completed", "items":[]
            })));
            let thread = Thread {
                metadata: serde_json::from_value(json!({
                    "id":"independent-scenario", "cwd":"/fixture", "fixtureHistory":kind
                }))
                .unwrap(),
                turns: vec![live],
            };
            let history = persisted(&thread).unwrap();
            assert_eq!(history.turns.len(), 16);
            assert_eq!(history.turns.last().unwrap().borrow()["id"], "live-turn");
            let last = history.turns[14].borrow();
            assert_eq!(last["status"], status);
            let item = last["items"].as_array().unwrap().last().unwrap();
            assert_eq!(item["phase"], phase);
            assert_eq!(
                item["text"],
                "Latest interrupted conversation message is visible."
            );
            if kind == "viewport" {
                assert!(history.turns[..14].iter().all(|turn| {
                    let turn = turn.borrow();
                    let items = turn["items"].as_array().unwrap();
                    !items.is_empty() && items.iter().all(|item| item["type"] == "commandExecution")
                }));
                assert_eq!(last["items"].as_array().unwrap().len(), 2);
                assert_eq!(last["items"][0]["type"], "userMessage");
            }
        }
        for kind in [Value::Null, json!("unknown")] {
            let thread = Thread {
                metadata: serde_json::from_value(json!({
                    "id":"fixture-long-history", "cwd":"/fixture", "fixtureHistory":kind
                }))
                .unwrap(),
                turns: vec![],
            };
            assert!(
                persisted(&thread).is_none(),
                "A session ID does not select a fixture's history"
            );
        }
    }
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
        let failure = context.home.join("fail-next-history-page");
        if params["cursor"].is_string() && failure.exists() {
            std::fs::remove_file(failure)?;
            return context.error(id, -32603, "fixture older history page failed");
        }
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
