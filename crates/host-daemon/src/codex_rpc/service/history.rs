use std::sync::Arc;

use codex_app_server::CodexAppServer;
use serde_json::{Value, json};

use super::{
    DispatchError, error_response, parse_params, raw_object, raw_value, replace_method,
    response_object, response_with_error, response_with_result,
};
use crate::DesktopProjectStore;

const MOBILE_THREAD_PAGE_SIZE: usize = 5;

/// Owns Host projections of Codex threads and their paginated history.
/// Session routing and approval ownership remain in the RPC service.
pub(super) struct ThreadHistory {
    app_server: Arc<CodexAppServer>,
    desktop_projects: DesktopProjectStore,
}

impl ThreadHistory {
    pub(super) fn new(
        app_server: Arc<CodexAppServer>,
        desktop_projects: DesktopProjectStore,
    ) -> Self {
        Self {
            app_server,
            desktop_projects,
        }
    }

    pub(super) async fn host_project_list(&self, line: &str) -> Result<String, DispatchError> {
        let params = parse_params(line)?;
        match self.desktop_projects.project_list(&params).await {
            Ok(result) => response_with_result(line, result),
            Err(error) => response_with_error(line, "desktop_project_state_unavailable", &error),
        }
    }

    pub(super) async fn host_title_list(&self, line: &str) -> Result<String, DispatchError> {
        let params = parse_params(line)?;
        let snapshot = match self.desktop_projects.load().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return response_with_error(line, "desktop_project_state_unavailable", &error);
            }
        };
        let mut projects = Vec::new();
        let mut project_cursor = Value::Null;
        loop {
            let mut page = snapshot
                .project_list(&json!({"limit":512,"cursor":project_cursor}))
                .map_err(|_| DispatchError::InvalidMessage("invalid project cursor".into()))?;
            projects.append(page["data"].as_array_mut().unwrap());
            project_cursor = page["nextCursor"].take();
            if project_cursor.is_null() {
                break;
            }
        }
        let mut titles = crate::desktop_projects::titles::TitleList::new(projects, &params);
        let mut cursors = std::collections::HashSet::new();
        let mut upstream_params = json!({"limit":100,"sortKey":"updated_at","sortDirection":"desc","useStateDbOnly":true});
        if let Some(term) = params
            .get("searchTerm")
            .and_then(Value::as_str)
            .filter(|term| !term.trim().is_empty())
        {
            upstream_params["searchTerm"] = json!(term);
        }
        loop {
            // The DB-only flag avoids scanning or repairing JSONL history. A
            // single Desktop snapshot gives every page the same membership rules.
            let response = match self
                .app_server
                .request_raw(
                    &json!({"id":0,"method":"thread/list","params":upstream_params}).to_string(),
                )
                .await
            {
                Ok(response) => response,
                Err(error) => return response_with_error(line, "codex_unavailable", &error),
            };
            let mut response: Value = serde_json::from_str(&response)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            if let Some(error) = response.get_mut("error") {
                let mut object = response_object(line)?;
                object.insert("error".into(), raw_value(error.take())?);
                return serde_json::to_string(&object)
                    .map_err(|error| DispatchError::InvalidMessage(error.to_string()));
            }
            let mut result = snapshot.enrich_threads(response["result"].take());
            let Some(data) = result["data"].as_array_mut() else {
                return error_response(line, "invalid_thread_list", "thread list data is missing");
            };
            for thread in data.drain(..) {
                titles.push(thread);
            }
            let next_cursor = result["nextCursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty());
            if titles.complete() || next_cursor.is_none() {
                break;
            }
            let next_cursor = next_cursor.unwrap();
            if !cursors.insert(next_cursor.to_owned()) {
                return error_response(line, "invalid_thread_list", "thread list cursor repeated");
            }
            upstream_params["cursor"] = json!(next_cursor);
        }
        response_with_result(line, titles.finish())
    }

    pub(super) async fn host_thread_request(
        &self,
        line: &str,
        upstream_method: &str,
        retain_recent_turns: bool,
        worktrees: &crate::worktrees::Worktrees,
    ) -> Result<String, DispatchError> {
        let mut upstream_line = replace_method(line, upstream_method)?;
        let mut params = parse_params(line)?;
        let worktree = if upstream_method == "thread/start" {
            match worktrees.prepare(params["cwd"].as_str()).await {
                Ok(worktree) => worktree,
                Err(error) => return response_with_error(line, "worktree_creation_failed", &error),
            }
        } else {
            None
        };
        if let Some(cwd) = &worktree {
            params["cwd"] = json!(cwd);
        }
        let paginate = params
            .as_object_mut()
            .and_then(|params| params.remove("paginateHistory"))
            == Some(Value::Bool(true));
        let defer_setting = params
            .as_object_mut()
            .and_then(|params| params.remove("deferItemDetails"));
        let defer_details = defer_setting == Some(Value::Bool(true));
        let hydrate = upstream_method == "thread/read" && params["includeTurns"] == true;
        if hydrate {
            params["includeTurns"] = Value::Bool(false);
        }
        if defer_setting.is_some() || hydrate || paginate || worktree.is_some() {
            let mut request = raw_object(&upstream_line)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            request.insert("params".into(), raw_value(params)?);
            upstream_line = serde_json::to_string(&request)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
        }
        let response = match self.app_server.request_raw(&upstream_line).await {
            Ok(response) => response,
            Err(error) => return response_with_error(line, "codex_unavailable", &error),
        };
        let mut object = raw_object(&response)
            .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
        let Some(raw_result) = object.get("result") else {
            // Error responses and future response shapes remain untouched.
            return Ok(response);
        };
        let mut result: Value = serde_json::from_str(raw_result.get())
            .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
        if hydrate {
            // Paginated threads expose history through the paging API. Metadata
            // stays available while the owning process has not flushed a rollout.
            let paginated = result["thread"]["historyMode"] == "paginated";
            let mut history_request = raw_object(&upstream_line)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            if paginated {
                history_request.insert("method".into(), raw_value(json!("thread/turns/list"))?);
                history_request.insert("params".into(), raw_value(json!({
                    "threadId":result["thread"]["id"], "limit":if paginate { MOBILE_THREAD_PAGE_SIZE } else { 10 },
                    "sortDirection":"desc", "itemsView":if paginate { "notLoaded" } else { "full" }
                }))?);
            } else {
                let mut params = parse_params(&upstream_line)?;
                params["includeTurns"] = Value::Bool(true);
                history_request.insert("params".into(), raw_value(params)?);
            }
            let history_request = serde_json::to_string(&history_request)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            let response = match self.app_server.request_raw(&history_request).await {
                Ok(response) => response,
                Err(error) => return response_with_error(line, "codex_unavailable", &error),
            };
            let mut history_object = raw_object(&response)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            let Some(history) = history_object.remove("result") else {
                return Ok(response);
            };
            let mut history: Value = serde_json::from_str(history.get())
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            if paginated {
                if paginate
                    && let Err(error) = self
                        .hydrate_turn_page(&mut history, &result["thread"]["id"])
                        .await
                {
                    return response_with_error(line, "invalid_thread_history", &error);
                }
                if let Err(message) = apply_native_turn_page(&mut result, history) {
                    return error_response(line, "invalid_thread_history", message);
                }
            } else {
                result = history;
                if !paginate
                    && let Some(turns) = result
                        .pointer_mut("/thread/turns")
                        .and_then(Value::as_array_mut)
                    && turns.len() > 10
                {
                    turns.drain(..turns.len() - 10);
                }
            }
        }
        let result = match self.desktop_projects.enrich_threads(result).await {
            Ok(result) => result,
            Err(error) => {
                return response_with_error(line, "desktop_project_state_unavailable", &error);
            }
        };
        let mut result = result;
        if retain_recent_turns && defer_details {
            defer_large_item_details(&mut result);
        }
        object.insert("result".to_owned(), raw_value(result)?);
        serde_json::to_string(&object)
            .map_err(|error| DispatchError::InvalidMessage(error.to_string()))
    }

    // Keep App Server cursors opaque. Both initial hydration and older pages
    // use the desktop five-turn / 500-item initial window, in pages of 100.
    async fn history_request(&self, method: &str, params: Value) -> Result<Value, String> {
        let request = json!({"id":0,"method":method,"params":params});
        let response = self
            .app_server
            .request_raw(&request.to_string())
            .await
            .map_err(|error| error.to_string())?;
        let mut response: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
        if let Some(error) = response.get("error") {
            return Err(error.to_string());
        }
        response
            .get_mut("result")
            .map(Value::take)
            .ok_or_else(|| "history result is missing".into())
    }

    async fn hydrate_turn_page(&self, page: &mut Value, thread_id: &Value) -> Result<(), String> {
        let turns = page["data"]
            .as_array_mut()
            .ok_or("turn page data is missing")?;
        if turns.len() > MOBILE_THREAD_PAGE_SIZE {
            return Err("turn page exceeds requested size".into());
        }
        let mut budget = MOBILE_THREAD_PAGE_SIZE * 100;
        for turn in turns {
            let mut values = Vec::with_capacity(budget);
            let mut cursor = Value::Null;
            let mut has_more = true;
            let mut cursors = std::collections::HashSet::new();
            let mut ids = std::collections::HashSet::new();
            while has_more && budget > 0 {
                let mut page = self
                    .history_request(
                        "thread/items/list",
                        json!({
                            "threadId":thread_id,"turnId":turn["id"],"cursor":cursor,
                            "limit":budget.min(100),"sortDirection":"desc"
                        }),
                    )
                    .await?;
                for item in take_history_items(&mut page)? {
                    if ids.insert(item["id"].as_str().unwrap().to_owned()) {
                        budget = budget
                            .checked_sub(1)
                            .ok_or("item page exceeds requested budget")?;
                        values.push(item);
                    }
                }
                cursor = page["nextCursor"].take();
                has_more = !cursor.is_null();
                if let Some(cursor) = cursor.as_str() {
                    if !cursors.insert(cursor.to_owned()) {
                        return Err("history cursor repeated".into());
                    }
                }
            }
            values.reverse();
            turn["items"] = Value::Array(values);
            turn["itemsNextCursor"] = cursor;
            turn["itemsHasMore"] = json!(has_more);
            turn["itemsView"] = json!(if has_more { "summary" } else { "full" });
            self.preserve_opening_question(turn, thread_id).await?;
        }
        Ok(())
    }

    async fn preserve_opening_question(
        &self,
        turn: &mut Value,
        thread_id: &Value,
    ) -> Result<(), String> {
        let values = turn["items"].as_array().ok_or("turn items are missing")?;
        if turn["itemsHasMore"] != true || values.is_empty() {
            return Ok(());
        }
        let mut opening = self
            .history_request(
                "thread/items/list",
                json!({
                    "threadId":thread_id,"turnId":turn["id"],"limit":2,"sortDirection":"asc"
                }),
            )
            .await?;
        if let Some(item) = take_history_items(&mut opening)?
            .into_iter()
            .find(|item| item["type"] != "contextCompaction")
            .filter(|item| item["type"] == "userMessage")
        {
            if !values.iter().any(|value| value["id"] == item["id"]) {
                turn["openingUserMessage"] = item;
            }
        }
        Ok(())
    }

    pub(super) async fn host_thread_history_page(
        &self,
        line: &str,
        items: bool,
    ) -> Result<String, DispatchError> {
        let params = parse_params(line)?;
        let Some(thread_id) = params["threadId"].as_str().filter(|id| !id.is_empty()) else {
            return error_response(line, "invalid_params", "threadId is required");
        };
        let cursor = params["cursor"].as_str().filter(|id| !id.is_empty());
        if !items && cursor.is_none() {
            return error_response(line, "invalid_params", "cursor is required");
        }
        let mut upstream = json!({"threadId":thread_id,"cursor":cursor,"sortDirection":"desc"});
        let method = if items {
            let Some(turn_id) = params["turnId"].as_str().filter(|id| !id.is_empty()) else {
                return error_response(line, "invalid_params", "turnId is required");
            };
            upstream["turnId"] = json!(turn_id);
            upstream["limit"] = json!(100);
            "thread/items/list"
        } else {
            upstream["limit"] = json!(MOBILE_THREAD_PAGE_SIZE);
            upstream["itemsView"] = json!("notLoaded");
            "thread/turns/list"
        };
        let mut page = match self.history_request(method, upstream).await {
            Ok(page) => page,
            Err(error) => return response_with_error(line, "codex_unavailable", &error),
        };
        if cursor.is_some() && page["nextCursor"].as_str() == cursor {
            return error_response(line, "invalid_thread_history", "history cursor repeated");
        }
        let mut result = json!({"thread":{"id":thread_id}});
        if items {
            let mut values = match take_history_items(&mut page) {
                Ok(values) => values,
                Err(error) => return response_with_error(line, "invalid_thread_history", &error),
            };
            values.reverse();
            result["thread"]["turns"] = json!([{"id":params["turnId"],"items":values,
                "itemsHasMore":!page["nextCursor"].is_null(),"itemsNextCursor":page["nextCursor"].take()}]);
            if cursor.is_none()
                && let Err(error) = self
                    .preserve_opening_question(&mut result["thread"]["turns"][0], &json!(thread_id))
                    .await
            {
                return response_with_error(line, "invalid_thread_history", &error);
            }
        } else {
            if let Err(error) = self.hydrate_turn_page(&mut page, &json!(thread_id)).await {
                return response_with_error(line, "invalid_thread_history", &error);
            }
            if let Err(error) = apply_native_turn_page(&mut result, page) {
                return error_response(line, "invalid_thread_history", error);
            }
        }
        if params["deferItemDetails"] == true {
            defer_large_item_details(&mut result);
        }
        response_with_result(line, result)
    }

    pub(super) async fn host_thread_item_read(&self, line: &str) -> Result<String, DispatchError> {
        let params = parse_params(line)?;
        let (Some(thread_id), Some(turn_id), Some(item_id)) = (
            params
                .get("threadId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty()),
            params
                .get("turnId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty()),
            params
                .get("itemId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty()),
        ) else {
            return error_response(
                line,
                "invalid_params",
                "threadId, turnId and itemId are required",
            );
        };
        let mut request = json!({"id":0,"method":"thread/items/list","params":{
            "threadId":thread_id,"turnId":turn_id,"limit":100,"sortDirection":"asc"
        }});
        let mut cursors = std::collections::HashSet::new();
        loop {
            let response = match self.app_server.request_raw(&request.to_string()).await {
                Ok(response) => response,
                Err(error) => return response_with_error(line, "codex_unavailable", &error),
            };
            let mut response: Value = serde_json::from_str(&response)
                .map_err(|error| DispatchError::InvalidMessage(error.to_string()))?;
            if let Some(error) = response.get_mut("error") {
                let mut object = response_object(line)?;
                object.insert("error".to_owned(), raw_value(error.take())?);
                return serde_json::to_string(&object)
                    .map_err(|error| DispatchError::InvalidMessage(error.to_string()));
            }
            let Some(entries) = response
                .pointer_mut("/result/data")
                .and_then(Value::as_array_mut)
            else {
                return error_response(line, "invalid_thread_history", "item page data is missing");
            };
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry["turnId"] == turn_id && entry["item"]["id"] == item_id)
            {
                return response_with_result(line, json!({"item":entry["item"].take()}));
            }
            let Some(cursor) = response
                .pointer("/result/nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
            else {
                return error_response(
                    line,
                    "item_not_found",
                    "The activity is no longer available. Refresh the task.",
                );
            };
            if !cursors.insert(cursor.to_owned()) {
                return error_response(line, "invalid_thread_history", "item page cursor repeated");
            }
            request["params"]["cursor"] = json!(cursor);
        }
    }
}

fn apply_native_turn_page(result: &mut Value, mut page: Value) -> Result<(), &'static str> {
    let Some(turns) = page["data"].as_array_mut() else {
        return Err("turn page data is missing");
    };
    turns.reverse();
    result["thread"]["turns"] = Value::Array(std::mem::take(turns));
    result["thread"]["historyCursor"] = page["nextCursor"]
        .as_str()
        .filter(|cursor| !cursor.is_empty())
        .map(|cursor| json!(cursor))
        .unwrap_or(Value::Null);
    Ok(())
}

fn take_history_items(page: &mut Value) -> Result<Vec<Value>, String> {
    let entries = page["data"]
        .as_array_mut()
        .ok_or("item page data is missing")?;
    if entries.len() > 100 {
        return Err("item page exceeds requested size".into());
    }
    for entry in entries.iter_mut() {
        let item = entry
            .get_mut("item")
            .ok_or("history item is missing")?
            .take();
        if item["id"].as_str().is_none_or(str::is_empty) {
            return Err("history item ID is missing".into());
        }
        *entry = item;
    }
    Ok(std::mem::take(entries))
}

// Count JSON bytes without materializing another copy of large tool output.
// The writer stops as soon as the inline budget is exceeded.
fn fits_inline(value: &Value) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("inline budget exceeded"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(4096), value).is_ok()
}

fn defer_large_item_details(result: &mut Value) {
    let Some(turns) = result
        .pointer_mut("/thread/turns")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for turn in turns {
        let Some(items) = turn.get_mut("items").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut deferred = Vec::new();
        for item in items {
            // Visible responses must remain complete, including image bytes and paths.
            // Only expandable activities defer their details.
            if matches!(
                item["type"].as_str(),
                Some("userMessage" | "agentMessage" | "imageGeneration")
            ) || fits_inline(item)
            {
                continue;
            }
            let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            deferred.push(Value::String(id.to_owned()));
            let Some(object) = item.as_object_mut() else {
                continue;
            };
            // File names/kinds and command/status keep the existing activity titles.
            if let Some(changes) = object.get_mut("changes").and_then(Value::as_array_mut) {
                for change in changes {
                    if let Some(change) = change.as_object_mut() {
                        change.remove("diff");
                    }
                }
            }
            object.retain(|key, value| {
                if key == "changes" || key == "id" || key == "type" {
                    return true;
                }
                if let Value::String(text) = value {
                    let mut end = text.len().min(256);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                }
                !value.is_array() && !value.is_object()
            });
        }
        if !deferred.is_empty() {
            turn["deferredItemIds"] = Value::Array(deferred);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_read_keeps_conversation_and_activity_headers() {
        let text = "会話".repeat(4096);
        let mut result = json!({"thread":{"turns":[{"id":"turn","items":[
            {"id":"user","type":"userMessage","content":[{"type":"text","text":text}]},
            {"id":"agent","type":"agentMessage","text":text},
            {"id":"command","type":"commandExecution","command":"日本語".repeat(1000),"status":"completed","aggregatedOutput":text},
            {"id":"files","type":"fileChange","status":"completed","changes":[{"path":"a.txt","kind":{"type":"update"},"diff":text}]},
            {"id":"future","type":"futureTool","tool":"inspect","status":"completed","result":{"content":text}},
            {"id":"small","type":"reasoning","summary":["short"]}
        ]}]}});
        defer_large_item_details(&mut result);
        let turn = &result["thread"]["turns"][0];
        let items = &turn["items"];
        assert_eq!(items[0]["content"][0]["text"], text);
        assert_eq!(items[1]["text"], text);
        assert!(items[2]["command"].as_str().unwrap().starts_with("日本語"));
        assert_eq!(items[2]["status"], "completed");
        assert_eq!(items[3]["changes"][0]["path"], "a.txt");
        assert_eq!(items[3]["changes"][0]["kind"]["type"], "update");
        assert_eq!(items[4]["tool"], "inspect");
        assert_eq!(items[5]["summary"], json!(["short"]));
        assert_eq!(
            turn["deferredItemIds"],
            json!(["command", "files", "future"])
        );
    }

    #[test]
    fn generated_image_output_is_not_truncated_as_an_activity_detail() {
        let image = json!({"id":"image","type":"imageGeneration","status":"completed",
            "result":"A".repeat(8192),"savedPath":format!("/{} image.png", "directory/".repeat(40))});
        let mut result = json!({"thread":{"turns":[{"items":[image]}]}});
        defer_large_item_details(&mut result);
        assert_eq!(result["thread"]["turns"][0]["items"][0], image);
        assert!(result["thread"]["turns"][0]["deferredItemIds"].is_null());
    }

    #[test]
    fn history_page_preserves_the_opaque_cursor_and_chronological_order() {
        let mut result = json!({"thread":{}});
        apply_native_turn_page(
            &mut result,
            json!({"data":[{"id":"new"},{"id":"old"}],"nextCursor":"opaque:token"}),
        )
        .unwrap();
        assert_eq!(result["thread"]["turns"][0]["id"], "old");
        assert_eq!(result["thread"]["historyCursor"], "opaque:token");
    }
}
