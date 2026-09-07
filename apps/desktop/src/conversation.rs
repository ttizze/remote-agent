use serde_json::{Value, json};
pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
#[derive(Default)]
pub(crate) struct Conversation {
    pub thread: Value,
    pub requests: Vec<Value>,
}
impl Conversation {
    pub fn reduce(&mut self, message: Value) {
        let method = text(&message, "method");
        let params = &message["params"];
        if method == "serverRequest/resolved" {
            self.requests.retain(|r| r["id"] != params["requestId"]);
            return;
        }
        if !method.is_empty() && message.get("id").is_some() {
            if let Some(old) = self.requests.iter_mut().find(|r| r["id"] == message["id"]) {
                *old = message;
            } else {
                self.requests.push(message);
            }
            return;
        }
        if self.thread.is_null()
            || (params.get("threadId").is_some() && params["threadId"] != self.thread["id"])
        {
            return;
        }
        if method == "thread/status/changed" {
            self.thread["status"] = params["status"].clone();
            return;
        }
        if !matches!(
            method,
            "turn/started"
                | "turn/completed"
                | "item/started"
                | "item/completed"
                | "item/agentMessage/delta"
                | "item/reasoning/textDelta"
                | "item/reasoning/summaryTextDelta"
                | "item/commandExecution/outputDelta"
                | "error"
        ) {
            return;
        }
        let id = params["turn"]["id"]
            .as_str()
            .or_else(|| params["turnId"].as_str())
            .unwrap_or("");
        if !self.thread["turns"].is_array() {
            self.thread["turns"] = json!([]);
        }
        let turns = self.thread["turns"].as_array_mut().unwrap();
        let ix = turns
            .iter()
            .position(|t| text(t, "id") == id)
            .unwrap_or_else(|| {
                turns.push(json!({"id":id,"status":"inProgress","items":[]}));
                turns.len() - 1
            });
        let turn = &mut turns[ix];
        if method.starts_with("turn/") {
            let incoming = &params["turn"];
            let preserve = matches!(text(incoming, "itemsView"), "summary" | "notLoaded")
                || array(&incoming["items"]).is_empty();
            if let Some(fields) = incoming.as_object() {
                for (key, value) in fields {
                    if key != "items" || !preserve {
                        turn[key] = value.clone();
                    }
                }
            }
            turn["status"] = json!(if method == "turn/started" {
                "inProgress"
            } else {
                incoming["status"].as_str().unwrap_or("completed")
            });
            if preserve {
                for item in array(&incoming["items"]) {
                    upsert(turn, item.clone());
                }
            }
        } else if matches!(method, "item/started" | "item/completed") {
            upsert(turn, params["item"].clone());
        } else if method == "error" {
            turn["error"] = params["error"].clone();
            turn["error"]["willRetry"] = json!(params["willRetry"] == true);
        } else {
            let (kind, field) = if method.contains("agentMessage") {
                ("agentMessage", "text")
            } else if method.contains("reasoning") {
                ("reasoning", "summary")
            } else {
                ("commandExecution", "aggregatedOutput")
            };
            let items = turn["items"].as_array_mut().unwrap();
            let ix = items
                .iter()
                .position(|i| i["id"] == params["itemId"])
                .unwrap_or_else(|| {
                    items.push(json!({"id":params["itemId"],"type":kind}));
                    items.len() - 1
                });
            let item = &mut items[ix];
            if !item[field].is_string() {
                item[field] = json!(if item[field].is_array() {
                    array(&item[field])
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    String::new()
                });
            }
            if let Value::String(value) = &mut item[field] {
                value.push_str(text(params, "delta"));
            }
        }
    }
    pub fn active(&self) -> Option<&Value> {
        array(&self.thread["turns"])
            .iter()
            .rev()
            .find(|t| t["status"] == "inProgress")
    }
}
fn upsert(turn: &mut Value, item: Value) {
    if !turn["items"].is_array() {
        turn["items"] = json!([]);
    }
    let items = turn["items"].as_array_mut().unwrap();
    if let Some(old) = items.iter_mut().find(|i| i["id"] == item["id"]) {
        *old = item;
    } else {
        items.push(item);
    }
}
pub(crate) struct Projection<'a> {
    pub users: Vec<&'a Value>,
    pub work: Vec<&'a Value>,
    pub finals: Vec<&'a Value>,
    pub collapsible: bool,
    pub thinking: bool,
    pub label: String,
}
pub(crate) fn project<'a>(turn: &'a Value, requests: &[Value]) -> Projection<'a> {
    let items = array(&turn["items"]);
    let mut finals: Vec<_> = items
        .iter()
        .filter(|i| i["type"] == "agentMessage" && i["phase"] == "final_answer")
        .collect();
    if finals.is_empty() {
        finals.extend(
            items
                .iter()
                .rev()
                .find(|i| i["type"] == "agentMessage" && i["phase"].is_null()),
        );
    }
    let users = items
        .iter()
        .filter(|i| i["type"] == "userMessage")
        .collect();
    let work: Vec<_> = items
        .iter()
        .filter(|i| {
            i["type"] != "userMessage"
                && !finals.iter().any(|f| f["id"] == i["id"])
                && !matches!(
                    text(i, "type"),
                    "sleep" | "enteredReviewMode" | "exitedReviewMode"
                )
        })
        .collect();
    let waiting = requests
        .iter()
        .any(|r| r["params"]["turnId"].is_null() || r["params"]["turnId"] == turn["id"]);
    let status = text(turn, "status");
    let seconds = turn["durationMs"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.)
        .map(|v| ((v / 1000.).round() as u64).max(1));
    let duration = seconds
        .map(|s| {
            if s < 60 {
                format!("{s}s")
            } else if s % 60 == 0 {
                format!("{}m", s / 60)
            } else {
                format!("{}m {}s", s / 60, s % 60)
            }
        })
        .unwrap_or_default();
    let label = if waiting {
        "確認を待っています".into()
    } else {
        match status {
            "inProgress" => {
                if duration.is_empty() {
                    "作業中…".into()
                } else {
                    format!("{duration} 作業中")
                }
            }
            "interrupted" => format!("{duration} 作業を停止しました"),
            "failed" => format!("{duration} 作業に失敗しました"),
            _ => {
                if duration.is_empty() {
                    "作業しました".into()
                } else {
                    format!("{duration}間作業しました")
                }
            }
        }
    };
    Projection {
        collapsible: status == "completed" && !finals.is_empty() && !waiting,
        thinking: status == "inProgress" && work.is_empty() && finals.is_empty() && !waiting,
        users,
        work,
        finals,
        label,
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
    fn completed_turn_projects_user_work_and_final() {
        let turn = json!({"id":"turn","status":"completed","durationMs":40000,"items":[{"id":"u","type":"userMessage"},{"id":"r","type":"reasoning"},{"id":"c","type":"commandExecution"},{"id":"a","type":"agentMessage","phase":"commentary"},{"id":"f","type":"agentMessage","phase":"final_answer"}]});
        let p = project(&turn, &[]);
        assert_eq!(p.users.len(), 1);
        assert_eq!(
            p.work.iter().map(|i| text(i, "id")).collect::<Vec<_>>(),
            ["r", "c", "a"]
        );
        assert_eq!(text(p.finals[0], "id"), "f");
        assert!(p.collapsible);
        assert!(!p.thinking);
        assert_eq!(p.label, "40s間作業しました");
    }
    #[test]
    fn completed_empty_turn_does_not_restore_thinking() {
        let turn = json!({"id":"turn","status":"completed","items":[{"id":"u","type":"userMessage","text":"追加メッセージ"}]});
        assert!(!project(&turn, &[]).thinking);
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
        let turn = json!({"id":"turn","status":"completed","items":[{"id":"a","type":"agentMessage","text":"done"},{"id":"c","type":"commandExecution"}]});
        assert!(!project(&turn, &state.requests).collapsible);
        state.reduce(json!({"method":"serverRequest/resolved","params":{"requestId":"r"}}));
        assert!(state.requests.is_empty());
        assert!(project(&turn, &state.requests).collapsible);
    }
}
