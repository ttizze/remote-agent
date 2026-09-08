use conversation_presentation::state::{
    CurrentMetadata, EventKind, EventMetadata, Mutation, classify_event, transition,
};
use serde_json::{Value, json};
pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

#[derive(Default, Debug)]
pub(crate) struct Change {
    pub turn: Option<usize>,
    pub item: Option<usize>,
    pub projection: bool,
    pub requests: bool,
    pub sources: bool,
    pub status: bool,
}
impl Change {
    pub fn changed(&self) -> bool {
        self.turn.is_some() || self.requests || self.status
    }
}

#[derive(Default)]
pub(crate) struct Conversation {
    pub thread: Value,
    pub requests: Vec<Value>,
}
impl Conversation {
    pub fn reduce(&mut self, mut message: Value) -> Change {
        let kind = classify_event(text(&message, "method"), message.get("id").is_some());
        let mut params = message["params"].take();
        let event = EventMetadata {
            kind,
            status: if kind == EventKind::GuardianReviewChanged {
                text(&params["review"], "status")
            } else {
                text(&params["turn"], "status")
            },
            has_error: params["turn"]
                .get("error")
                .is_some_and(|error| !error.is_null()),
            will_retry: params["willRetry"] == true,
            empty_delta: text(&params, "delta").is_empty(),
        };
        if matches!(kind, EventKind::RequestStarted | EventKind::RequestResolved) {
            let action = transition(&event, &CurrentMetadata::default()).action;
            match action {
                Mutation::Request => {
                    message["params"] = params;
                    if let Some(old) = self.requests.iter_mut().find(|r| r["id"] == message["id"]) {
                        *old = message;
                    } else {
                        self.requests.push(message);
                    }
                }
                Mutation::ResolveRequest => {
                    self.requests.retain(|r| r["id"] != params["requestId"])
                }
                _ => unreachable!(),
            }
            return Change {
                requests: true,
                ..Default::default()
            };
        }
        if self.thread.is_null()
            || params
                .get("threadId")
                .is_some_and(|id| id != &self.thread["id"])
        {
            return Change::default();
        }
        let id = params["turn"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or_else(|| params["turnId"].as_str())
            .unwrap_or("");
        let turn_index = array(&self.thread["turns"])
            .iter()
            .rposition(|turn| text(turn, "id") == id);
        let current = turn_index.map(|i| &self.thread["turns"][i]);
        let item_id = if kind == EventKind::GuardianReviewChanged {
            &params["reviewId"]
        } else if matches!(kind, EventKind::ItemStarted | EventKind::ItemCompleted) {
            &params["item"]["id"]
        } else {
            &params["itemId"]
        };
        let item_index = current.and_then(|turn| {
            array(&turn["items"])
                .iter()
                .position(|item| item["id"] == *item_id)
        });
        let decision = transition(
            &event,
            &CurrentMetadata {
                turn_status: current.and_then(|turn| turn["status"].as_str()),
                item_type: current
                    .and_then(|turn| item_index.and_then(|i| turn["items"][i]["type"].as_str())),
                retrying_error: current.is_some_and(|turn| turn["error"]["willRetry"] == true),
            },
        );
        if decision.action == Mutation::Ignore {
            return Change::default();
        }
        if decision.action == Mutation::ThreadStatus {
            if self.thread["status"] == params["status"] {
                return Change::default();
            }
            self.thread["status"] = params["status"].take();
            return Change {
                status: true,
                ..Default::default()
            };
        }
        let ix = match turn_index {
            Some(index) => index,
            None if decision.action == Mutation::Turn && !id.is_empty() => {
                let turn = json!({"id":id,"status":"inProgress","items":[]});
                if !self.thread["turns"].is_array() {
                    self.thread["turns"] = json!([]);
                }
                let turns = self.thread["turns"].as_array_mut().unwrap();
                turns.push(turn);
                turns.len() - 1
            }
            _ => return Change::default(),
        };
        let turn = &mut self.thread["turns"][ix];
        let mut change = Change {
            turn: Some(ix),
            item: item_index,
            ..Default::default()
        };
        match decision.action {
            Mutation::Turn => {
                let mut incoming = params["turn"].take();
                let preserve = matches!(text(&incoming, "itemsView"), "summary" | "notLoaded")
                    || array(&incoming["items"]).is_empty();
                let items = incoming["items"].take();
                if let Value::Object(fields) = incoming {
                    for (key, value) in fields {
                        if key != "items"
                            && key != "status"
                            && !(value.is_null()
                                && matches!(
                                    key.as_str(),
                                    "error" | "startedAt" | "completedAt" | "durationMs"
                                ))
                        {
                            turn[key] = value;
                        }
                    }
                }
                if let Value::Array(items) = items {
                    for item in &items {
                        remove_deferred(turn, text(item, "id"));
                    }
                    if preserve {
                        for item in items {
                            upsert(turn, item);
                        }
                    } else {
                        turn["items"] = Value::Array(items);
                    }
                }
                turn["status"] = json!(decision.status.unwrap());
                if decision.clear_error {
                    turn.as_object_mut().unwrap().remove("error");
                }
                change.projection = true;
            }
            Mutation::Item => {
                let item = if kind == EventKind::GuardianReviewChanged {
                    params["id"] = params["reviewId"].take();
                    params["type"] = json!("automaticApprovalReview");
                    params
                } else {
                    params["item"].take()
                };
                change.sources = item["type"] == "userMessage";
                change.projection =
                    item_index.is_none_or(|index| !same_presentation(&turn["items"][index], &item));
                remove_deferred(turn, text(&item, "id"));
                change.item = Some(upsert(turn, item));
            }
            Mutation::RemoveItem => {
                let Some(index) = item_index else {
                    return Change::default();
                };
                turn["items"].as_array_mut().unwrap().remove(index);
                change.projection = true;
            }
            Mutation::Error => {
                let mut error = params["error"].take();
                if !error.is_object() {
                    error = json!({"message":error});
                }
                error["willRetry"] = params["willRetry"].take();
                turn["error"] = error;
            }
            Mutation::Append => {
                let item = &mut turn["items"][item_index.unwrap()];
                let field = decision.field.unwrap();
                let value = if field == "diff" {
                    if !item["changes"].is_array() {
                        item["changes"] = json!([]);
                    }
                    let changes = item["changes"].as_array_mut().unwrap();
                    if changes.is_empty() {
                        changes.push(json!({"path":"","kind":"update","diff":""}));
                        change.projection = true;
                    }
                    &mut changes.last_mut().unwrap()["diff"]
                } else {
                    &mut item[field]
                };
                append_text(value, text(&params, "delta"));
            }
            _ => unreachable!(),
        }
        change
    }
    pub fn active(&self) -> Option<&Value> {
        array(&self.thread["turns"])
            .iter()
            .rev()
            .find(|t| t["status"] == "inProgress")
    }
}
fn append_text(value: &mut Value, delta: &str) {
    if !value.is_string() {
        let original = value.take();
        *value = Value::String(match original {
            Value::Array(parts) => {
                let mut text = String::new();
                for part in parts {
                    if let Value::String(part) = part {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&part);
                    }
                }
                text
            }
            _ => String::new(),
        });
    }
    if let Value::String(text) = value {
        text.push_str(delta);
    }
}
fn same_presentation(a: &Value, b: &Value) -> bool {
    a["type"] == b["type"]
        && a["phase"] == b["phase"]
        && a["clientId"] == b["clientId"]
        && array(&a["changes"]).len() == array(&b["changes"]).len()
}
fn upsert(turn: &mut Value, item: Value) -> usize {
    if !turn["items"].is_array() {
        turn["items"] = json!([]);
    }
    let items = turn["items"].as_array_mut().unwrap();
    if let Some(index) = items.iter().position(|old| old["id"] == item["id"]) {
        items[index] = item;
        index
    } else {
        let index = items.len();
        items.push(item);
        index
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryPage {
    pub turn: Option<String>,
    pub cursor: Value,
}
impl Conversation {
    pub fn older_page(&self) -> Option<HistoryPage> {
        for turn in array(&self.thread["turns"]) {
            if turn["itemsHasMore"] == true {
                return Some(HistoryPage {
                    turn: Some(text(turn, "id").into()),
                    cursor: turn["itemsNextCursor"].clone(),
                });
            }
        }
        self.thread["historyCursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|_| HistoryPage {
                turn: None,
                cursor: self.thread["historyCursor"].clone(),
            })
    }
    // Check the opaque cursor before mutation; a delayed page cannot replay or
    // overwrite content observed through live events or a newer read.
    pub fn merge_older(&mut self, mut page: Value, request: &HistoryPage) -> Result<usize, String> {
        if page["thread"]["id"] != self.thread["id"] {
            return Err("履歴の会話IDが一致しません".into());
        }
        let incoming = page["thread"]["turns"]
            .as_array_mut()
            .ok_or("履歴のターンがありません")?;
        if let Some(id) = &request.turn {
            if incoming.len() != 1 || incoming[0]["id"] != *id {
                return Err("履歴のターンIDが一致しません".into());
            }
            let turn = self.thread["turns"]
                .as_array_mut()
                .and_then(|turns| turns.iter_mut().find(|turn| turn["id"] == *id))
                .ok_or("履歴のターンが見つかりません")?;
            if turn["itemsNextCursor"] != request.cursor || turn["itemsHasMore"] != true {
                return Ok(0);
            }
            let older = &mut incoming[0];
            if !older["itemsNextCursor"].is_null() && older["itemsNextCursor"] == request.cursor {
                return Err("履歴カーソルが進みませんでした".into());
            }
            let values = older["items"]
                .as_array_mut()
                .ok_or("履歴の項目がありません")?;
            if values.iter().any(|item| text(item, "id").is_empty()) {
                return Err("履歴の項目IDがありません".into());
            }
            let mut prefix = std::mem::take(values);
            let mut known: std::collections::HashSet<String> = array(&turn["items"])
                .iter()
                .map(|item| text(item, "id").to_owned())
                .collect();
            prefix.retain(|item| known.insert(text(item, "id").to_owned()));
            let mut deferred = array(&turn["deferredItemIds"]).to_vec();
            for id in array(&older["deferredItemIds"]) {
                if prefix.iter().any(|item| &item["id"] == id) && !deferred.contains(id) {
                    deferred.push(id.clone());
                }
            }
            if let Some(items) = turn["items"].as_array_mut() {
                prefix.append(items);
            }
            turn["items"] = Value::Array(prefix);
            turn["deferredItemIds"] = Value::Array(deferred);
            turn["itemsNextCursor"] = older["itemsNextCursor"].take();
            turn["itemsHasMore"] = json!(older["itemsHasMore"] == true);
            if !older["openingUserMessage"].is_null() {
                turn["openingUserMessage"] = older["openingUserMessage"].take();
            }
            Ok(0)
        } else {
            if self.thread["historyCursor"] != request.cursor {
                return Ok(0);
            }
            if incoming
                .iter()
                .any(|turn| text(turn, "id").is_empty() || !turn["items"].is_array())
            {
                return Err("履歴のターンが不正です".into());
            }
            let mut prefix = std::mem::take(incoming);
            if !page["thread"]["historyCursor"].is_null()
                && page["thread"]["historyCursor"] == request.cursor
            {
                return Err("履歴カーソルが進みませんでした".into());
            }
            // Repeated turn IDs within a page are native history, not duplicates.
            prefix.retain(|turn| {
                !array(&self.thread["turns"])
                    .iter()
                    .any(|current| current["id"] == turn["id"])
            });
            let added = prefix.len();
            if let Some(turns) = self.thread["turns"].as_array_mut() {
                prefix.append(turns);
            }
            self.thread["turns"] = Value::Array(prefix);
            self.thread["historyCursor"] = page["thread"]["historyCursor"].take();
            Ok(added)
        }
    }
    pub fn apply_detail(
        &mut self,
        turn_id: &str,
        item_id: &str,
        item: Value,
    ) -> Result<bool, String> {
        if text(&item, "id") != item_id {
            return Err("詳細の項目IDが一致しません".into());
        }
        let Some(turn) = self.thread["turns"]
            .as_array_mut()
            .and_then(|turns| turns.iter_mut().find(|turn| turn["id"] == turn_id))
        else {
            return Ok(false);
        };
        if !array(&turn["deferredItemIds"])
            .iter()
            .any(|id| id == item_id)
        {
            return Ok(false);
        }
        let Some(target) = turn["items"]
            .as_array_mut()
            .and_then(|items| items.iter_mut().find(|item| item["id"] == item_id))
        else {
            return Ok(false);
        };
        *target = item;
        remove_deferred(turn, item_id);
        Ok(true)
    }
}
fn remove_deferred(turn: &mut Value, item_id: &str) {
    if let Some(ids) = turn["deferredItemIds"].as_array_mut() {
        ids.retain(|id| id != item_id);
    }
}

impl Conversation {
    pub fn refresh_history(&mut self, mut fresh: Value) -> Result<(), String> {
        if fresh["id"] != self.thread["id"] || !fresh["turns"].is_array() {
            return Err("更新された履歴が不正です".into());
        }
        let first = array(&fresh["turns"]).first().map(|turn| text(turn, "id"));
        let boundary = first.and_then(|id| {
            array(&self.thread["turns"])
                .iter()
                .position(|turn| text(turn, "id") == id)
        });
        if self.thread.get("historyCursor").is_some()
            && let Some(boundary) = boundary
        {
            let mut old = self.thread["turns"]
                .take()
                .as_array_mut()
                .map(std::mem::take)
                .unwrap_or_default();
            let tail = old.split_off(boundary);
            let mut old_tail = tail;
            for turn in fresh["turns"].as_array_mut().unwrap() {
                let Some(previous) = old_tail
                    .iter_mut()
                    .find(|previous| previous["id"] == turn["id"])
                else {
                    continue;
                };
                let mut previous = previous.take();
                let item_boundary = array(&turn["items"]).first().and_then(|first| {
                    array(&previous["items"])
                        .iter()
                        .position(|item| item["id"] == first["id"])
                });
                if let Some(item_boundary) = item_boundary {
                    let mut prefix = previous["items"]
                        .take()
                        .as_array_mut()
                        .map(std::mem::take)
                        .unwrap_or_default();
                    let mut old_items = prefix.split_off(item_boundary);
                    let mut deferred = Vec::new();
                    for id in array(&previous["deferredItemIds"]) {
                        if prefix.iter().any(|item| item["id"] == *id) {
                            deferred.push(id.clone());
                        }
                    }
                    // Keep already fetched details when a fresh tail only carries summaries.
                    let fresh_deferred = turn["deferredItemIds"].take();
                    for item in turn["items"].as_array_mut().unwrap() {
                        if array(&fresh_deferred).iter().any(|id| *id == item["id"]) {
                            if !array(&previous["deferredItemIds"])
                                .iter()
                                .any(|id| *id == item["id"])
                                && let Some(full) =
                                    old_items.iter_mut().find(|old| old["id"] == item["id"])
                            {
                                *item = full.take();
                            } else {
                                deferred.push(item["id"].clone());
                            }
                        }
                    }
                    prefix.append(turn["items"].as_array_mut().unwrap());
                    turn["items"] = Value::Array(prefix);
                    turn["deferredItemIds"] = Value::Array(deferred);
                    turn["itemsHasMore"] = previous["itemsHasMore"].take();
                    turn["itemsNextCursor"] = previous["itemsNextCursor"].take();
                    if turn["openingUserMessage"].is_null() {
                        turn["openingUserMessage"] = previous["openingUserMessage"].take();
                    }
                } else if array(&turn["items"]).is_empty() && turn["itemsHasMore"] == true {
                    for field in [
                        "items",
                        "itemsHasMore",
                        "itemsNextCursor",
                        "deferredItemIds",
                        "openingUserMessage",
                    ] {
                        turn[field] = previous[field].take();
                    }
                }
            }
            old.append(fresh["turns"].as_array_mut().unwrap());
            fresh["turns"] = Value::Array(old);
            fresh["historyCursor"] = self.thread["historyCursor"].take();
        }
        self.thread = fresh;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_completion_preserves_streamed_items() {
        let items = json!([{"id":"user","type":"userMessage","content":[{"type":"text","text":"edit file"}]},{"id":"command","type":"commandExecution","status":"completed"},{"id":"answer","type":"agentMessage","phase":"final_answer","text":"done"}]);
        let mut state = Conversation {
            thread: json!({"id":"thread","turns":[{"id":"turn","status":"inProgress","items":items}]}),
            requests: vec![],
        };
        state.reduce(json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed","itemsView":"summary","items":[items[2]],"durationMs":2349}}}));
        assert_eq!(state.thread["turns"][0]["items"], items);
        assert_eq!(state.thread["turns"][0]["durationMs"], 2349);
        assert!(state.active().is_none());
    }
    #[test]
    fn deltas_and_completion_update_the_same_item() {
        let mut state = Conversation {
            thread: json!({"id":"thread","turns":[]}),
            requests: vec![],
        };
        for event in [
            json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"turn","items":[]}}}),
            json!({"method":"item/started","params":{"threadId":"thread","turnId":"turn","item":{"id":"agent","type":"agentMessage","phase":"commentary","text":""}}}),
            json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"agent","delta":"確認しています"}}),
        ] {
            state.reduce(event);
        }
        assert_eq!(
            state.thread["turns"][0]["items"][0]["text"],
            "確認しています"
        );
        state.reduce(json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"agent","type":"agentMessage","phase":"commentary","text":"確認しています。","unknownFutureField":42}}}));
        let items = array(&state.thread["turns"][0]["items"]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "確認しています。");
        assert_eq!(items[0]["unknownFutureField"], 42);
        state.reduce(json!({"method":"item/agentMessage/delta","params":{"threadId":"other","turnId":"turn","itemId":"agent","delta":"wrong"}}));
        assert_eq!(
            state.thread["turns"][0]["items"][0]["text"],
            "確認しています。"
        );
    }
    #[test]
    fn requests_remain_actionable_until_resolved() {
        let mut state = Conversation::default();
        let request = json!({"id":"r","method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","command":"npm test"}});
        state.reduce(request.clone());
        state.reduce(request.clone());
        assert_eq!(state.requests, vec![request]);
        state.reduce(json!({"method":"serverRequest/resolved","params":{"requestId":"r"}}));
        assert!(state.requests.is_empty());
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    fn conversation() -> Conversation {
        Conversation {
            thread: json!({"id":"thread","turns":[{"id":"turn","status":"inProgress","items":[]}]}),
            requests: vec![],
        }
    }
    #[test]
    fn denied_automatic_review_is_visible_until_approved() {
        let mut state = conversation();
        let event = |status| json!({"method":"item/autoApprovalReview/completed","params":{"threadId":"thread","turnId":"turn","reviewId":"review","review":{"status":status}}});
        state.reduce(event("denied"));
        assert_eq!(
            state.thread["turns"][0]["items"][0]["type"],
            "automaticApprovalReview"
        );
        assert_eq!(
            state.thread["turns"][0]["items"][0]["review"]["status"],
            "denied"
        );
        state.reduce(event("approved"));
        assert!(array(&state.thread["turns"][0]["items"]).is_empty());
    }
    #[test]
    fn completed_item_moves_its_owned_body_without_copying_it() {
        let mut state = conversation();
        let message = json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"answer","type":"agentMessage","text":"x".repeat(1024*1024)}}});
        let body = message["params"]["item"]["text"].as_str().unwrap().as_ptr();
        state.reduce(message);
        assert_eq!(
            state.thread["turns"][0]["items"][0]["text"]
                .as_str()
                .unwrap()
                .as_ptr(),
            body
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    fn state() -> Conversation {
        Conversation {
            thread: json!({"id":"thread","historyCursor":"turns:1","turns":[{"id":"turn","status":"inProgress","itemsHasMore":true,"itemsNextCursor":"items:1","items":[{"id":"answer","type":"agentMessage","text":"live answer"}]}]}),
            requests: vec![],
        }
    }
    #[test]
    fn older_items_keep_live_values_and_do_not_redefer_them() {
        let mut state = state();
        let body = state.thread["turns"][0]["items"][0]["text"]
            .as_str()
            .unwrap()
            .as_ptr();
        let request = state.older_page().unwrap();
        state.merge_older(json!({"thread":{"id":"thread","turns":[{"id":"turn","items":[{"id":"command","type":"commandExecution"},{"id":"answer","type":"agentMessage","text":"stale"}],"deferredItemIds":["command","answer"],"itemsHasMore":false,"itemsNextCursor":null}]}}), &request).unwrap();
        let turn = &state.thread["turns"][0];
        assert_eq!(turn["status"], "inProgress");
        assert_eq!(turn["items"][1]["text"], "live answer");
        assert_eq!(turn["items"][1]["text"].as_str().unwrap().as_ptr(), body);
        assert_eq!(turn["deferredItemIds"], json!(["command"]));
        assert_eq!(state.older_page().unwrap().turn, None);
        state.merge_older(json!({"thread":{"id":"thread","turns":[{"id":"turn","items":[{"id":"duplicate"}]}]}}), &request).unwrap();
        assert_eq!(array(&state.thread["turns"][0]["items"]).len(), 2);
    }
    #[test]
    fn pages_reject_wrong_targets_and_nonadvancing_cursors_without_mutation() {
        let mut state = state();
        let request = state.older_page().unwrap();
        let before = state.thread.clone();
        for page in [
            json!({"thread":{"id":"other","turns":[]}}),
            json!({"thread":{"id":"thread","turns":[{"id":"other","items":[]}]}}),
            json!({"thread":{"id":"thread","turns":[{"id":"turn","items":[],"itemsNextCursor":"items:1"}]}}),
        ] {
            assert!(state.merge_older(page, &request).is_err());
            assert_eq!(state.thread, before);
        }
    }
    #[test]
    fn older_turns_preserve_native_repeated_ids_and_skip_loaded_overlap() {
        let mut state = state();
        let request = HistoryPage {
            turn: None,
            cursor: json!("turns:1"),
        };
        let added = state.merge_older(json!({"thread":{"id":"thread","historyCursor":null,"turns":[{"id":"repeat","items":[{"id":"one"}]},{"id":"repeat","items":[{"id":"two"}]},{"id":"turn","items":[]}]}}), &request).unwrap();
        assert_eq!(added, 2);
        assert_eq!(state.thread["turns"][0]["items"][0]["id"], "one");
        assert_eq!(state.thread["turns"][1]["items"][0]["id"], "two");
        assert_eq!(state.thread["turns"][2]["items"][0]["text"], "live answer");
    }
    #[test]
    fn late_detail_cannot_replace_an_item_completed_live() {
        let mut state = state();
        state.thread["turns"][0]["deferredItemIds"] = json!(["answer"]);
        state.reduce(json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"answer","type":"agentMessage","text":"new full body"}}}));
        assert!(
            !state
                .apply_detail("turn", "answer", json!({"id":"answer","text":"stale"}))
                .unwrap()
        );
        assert_eq!(
            state.thread["turns"][0]["items"][0]["text"],
            "new full body"
        );
    }
    #[test]
    fn refresh_keeps_loaded_prefix_cursor_and_fetched_detail() {
        let mut state = state();
        state.thread["turns"][0]["items"] = json!([{"id":"prefix","type":"userMessage"},{"id":"command","type":"commandExecution","aggregatedOutput":"full fetched output"},{"id":"answer","type":"agentMessage","text":"old"}]);
        let body = state.thread["turns"][0]["items"][1]["aggregatedOutput"]
            .as_str()
            .unwrap()
            .as_ptr();
        state.refresh_history(json!({"id":"thread","historyCursor":"new-window","turns":[{"id":"turn","status":"completed","itemsHasMore":true,"itemsNextCursor":"new-items-window","deferredItemIds":["command"],"items":[{"id":"command","type":"commandExecution"},{"id":"answer","type":"agentMessage","text":"updated"}]}]})).unwrap();
        let turn = &state.thread["turns"][0];
        assert_eq!(state.thread["historyCursor"], "turns:1");
        assert_eq!(turn["itemsNextCursor"], "items:1");
        assert_eq!(turn["items"][0]["id"], "prefix");
        assert_eq!(
            turn["items"][1]["aggregatedOutput"]
                .as_str()
                .unwrap()
                .as_ptr(),
            body
        );
        assert_eq!(turn["items"][2]["text"], "updated");
        assert_eq!(turn["deferredItemIds"], json!([]));
    }
    #[test]
    fn delta_dirties_only_its_row_but_phase_change_invalidates_projection() {
        let mut state = state();
        let change = state.reduce(json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"answer","delta":"!"}}));
        assert_eq!(change.turn, Some(0));
        assert!(!change.projection);
        let change = state.reduce(json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"answer","type":"agentMessage","phase":"final_answer","text":"done"}}}));
        assert!(change.projection);
        assert_eq!(array(&state.thread["turns"][0]["items"]).len(), 1);
    }
}
