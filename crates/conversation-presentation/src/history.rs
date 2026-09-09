//! Owned history reconciliation shared by desktop and native metadata adapters.
//! Bodies move once; repeated turn IDs retain their separate occurrences.
use crate::{array, text};
use serde_json::{Value, json};
use std::collections::HashSet;

fn deferred(turn: &Value, item: &Value) -> bool {
    array(&turn["deferredItemIds"])
        .iter()
        .any(|id| *id == item["id"])
}

fn has_more(turn: &Value) -> bool {
    turn["itemsHasMore"]
        .as_bool()
        .unwrap_or_else(|| turn["itemsNextCursor"].as_str().is_some())
}

fn take_array(value: &mut Value) -> Vec<Value> {
    match value.take() {
        Value::Array(values) => values,
        _ => Vec::new(),
    }
}

fn prepend_items(turn: &mut Value, older: &mut Value) {
    let previous = take_array(&mut turn["items"]);
    let incoming = take_array(&mut older["items"]);
    let mut known: HashSet<&str> = previous.iter().map(|item| text(item, "id")).collect();
    // Keep borrowed IDs while deciding which page entries survive. Only new
    // entries need a bit; no source/index plan or copies of IDs are retained.
    let include: Vec<bool> = incoming
        .iter()
        .map(|item| known.insert(text(item, "id")))
        .collect();
    drop(known);
    let mut items = Vec::with_capacity(previous.len() + incoming.len());
    let mut ids = Vec::new();
    for (item, source) in incoming
        .into_iter()
        .zip(include)
        .filter_map(|(item, keep)| keep.then_some((item, &*older)))
        .chain(previous.into_iter().map(|item| (item, &*turn)))
    {
        if deferred(source, &item) {
            ids.push(item["id"].clone());
        }
        items.push(item);
    }
    turn["items"] = Value::Array(items);
    turn["deferredItemIds"] = Value::Array(ids);
    turn["itemsHasMore"] = json!(has_more(older));
    turn["itemsNextCursor"] = older["itemsNextCursor"].take();
    if !older["openingUserMessage"].is_null() {
        turn["openingUserMessage"] = older["openingUserMessage"].take();
    }
}

/// Validate before changing state; delayed pages cannot overwrite live content.
pub fn merge_older(
    mut previous: Value,
    mut page: Value,
    turn_id: Option<&str>,
    cursor: &Value,
) -> (Value, Result<usize, String>) {
    let result = (|| {
        if page["thread"]["id"] != previous["id"] {
            return Err("履歴の会話IDが一致しません".into());
        }
        let incoming = page["thread"]["turns"]
            .as_array_mut()
            .ok_or("履歴のターンがありません")?;
        if let Some(id) = turn_id {
            if incoming.len() != 1 || incoming[0]["id"] != id {
                return Err("履歴のターンIDが一致しません".into());
            }
            let turn = previous["turns"]
                .as_array_mut()
                .and_then(|turns| turns.iter_mut().find(|turn| turn["id"] == id))
                .ok_or("履歴のターンが見つかりません")?;
            if turn["itemsNextCursor"] != *cursor || !has_more(turn) {
                return Ok(0);
            }
            let older = &mut incoming[0];
            if !older["itemsNextCursor"].is_null() && older["itemsNextCursor"] == *cursor {
                return Err("履歴カーソルが進みませんでした".into());
            }
            if older["items"]
                .as_array()
                .ok_or("履歴の項目がありません")?
                .iter()
                .any(|item| text(item, "id").is_empty())
            {
                return Err("履歴の項目IDがありません".into());
            }
            prepend_items(turn, older);
            Ok(0)
        } else {
            if previous["historyCursor"] != *cursor {
                return Ok(0);
            }
            if incoming
                .iter()
                .any(|turn| text(turn, "id").is_empty() || !turn["items"].is_array())
            {
                return Err("履歴のターンが不正です".into());
            }
            if !page["thread"]["historyCursor"].is_null()
                && page["thread"]["historyCursor"] == *cursor
            {
                return Err("履歴カーソルが進みませんでした".into());
            }
            let old = take_array(&mut previous["turns"]);
            let new = take_array(&mut page["thread"]["turns"]);
            let known: HashSet<&str> = old.iter().map(|turn| text(turn, "id")).collect();
            let mut turns = Vec::with_capacity(old.len() + new.len());
            // Page-local duplicate turn IDs represent separate native occurrences.
            turns.extend(
                new.into_iter()
                    .filter(|turn| !known.contains(text(turn, "id"))),
            );
            let added = turns.len();
            drop(known);
            turns.extend(old);
            previous["turns"] = Value::Array(turns);
            previous["historyCursor"] = page["thread"]["historyCursor"].take();
            Ok(added)
        }
    })();
    (previous, result)
}

fn refresh_items(previous: &mut Value, fresh: &mut Value) {
    let old = array(&previous["items"]);
    let new = array(&fresh["items"]);
    let boundary = if new.is_empty() {
        if !has_more(fresh) {
            return;
        }
        old.len()
    } else {
        let Some(index) = old.iter().position(|item| item["id"] == new[0]["id"]) else {
            return;
        };
        index
    };
    let mut old = take_array(&mut previous["items"]);
    let new = take_array(&mut fresh["items"]);
    let mut items = Vec::with_capacity(boundary + new.len());
    let mut ids = Vec::new();
    for item in &mut old[..boundary] {
        if deferred(previous, item) {
            ids.push(item["id"].clone());
        }
        items.push(item.take());
    }
    let mut consumed = vec![false; old.len() - boundary];
    for item in new {
        let matched = old[boundary..]
            .iter()
            .enumerate()
            .find(|(index, value)| !consumed[*index] && value["id"] == item["id"])
            .map(|(index, _)| index);
        if let Some(index) = matched {
            consumed[index] = true;
        }
        let is_deferred = deferred(fresh, &item);
        if let Some(index) =
            matched.filter(|index| is_deferred && !deferred(previous, &old[boundary + index]))
        {
            items.push(old[boundary + index].take());
        } else {
            if is_deferred {
                ids.push(item["id"].clone());
            }
            items.push(item);
        }
    }
    fresh["items"] = Value::Array(items);
    fresh["deferredItemIds"] = Value::Array(ids);
    fresh["itemsHasMore"] = json!(has_more(previous));
    fresh["itemsNextCursor"] = previous["itemsNextCursor"].take();
    if fresh["openingUserMessage"].is_null() && !previous["openingUserMessage"].is_null() {
        fresh["openingUserMessage"] = previous["openingUserMessage"].take();
    }
}

fn retain_requests(previous: &mut Value, fresh: &mut Value) {
    if array(&previous["pendingRequests"]).is_empty() {
        return;
    }
    // Reads contain persisted history, not the live request-resolution stream.
    let mut requests = take_array(&mut previous["pendingRequests"]);
    for request in take_array(&mut fresh["pendingRequests"]) {
        if let Some(existing) = requests
            .iter_mut()
            .find(|value| value["id"] == request["id"])
        {
            *existing = request;
        } else {
            requests.push(request);
        }
    }
    fresh["pendingRequests"] = Value::Array(requests);
}

pub fn merge_refresh(mut previous: Value, mut fresh: Value) -> (Value, Result<(), String>) {
    if fresh["id"] != previous["id"] || !fresh["turns"].is_array() {
        return (previous, Err("更新された履歴が不正です".into()));
    }
    let boundary =
        if previous.get("historyCursor").is_some() && fresh.get("historyCursor").is_some() {
            array(&fresh["turns"]).first().and_then(|first| {
                array(&previous["turns"])
                    .iter()
                    .position(|turn| turn["id"] == first["id"])
            })
        } else {
            None
        };
    if boundary.is_none()
        && !array(&previous["turns"])
            .iter()
            .any(|turn| !array(&turn["pendingRequests"]).is_empty())
    {
        return (fresh, Ok(()));
    }
    let retain_history = boundary.is_some();
    let boundary = boundary.unwrap_or_default();
    let mut old = take_array(&mut previous["turns"]);
    let new = take_array(&mut fresh["turns"]);
    let mut turns = Vec::with_capacity(boundary + new.len());
    turns.extend(old[..boundary].iter_mut().map(Value::take));
    let mut consumed = vec![false; old.len() - boundary];
    for mut turn in new {
        if let Some((index, previous_turn)) = old[boundary..]
            .iter_mut()
            .enumerate()
            .find(|(index, previous)| !consumed[*index] && previous["id"] == turn["id"])
        {
            consumed[index] = true;
            if retain_history {
                refresh_items(previous_turn, &mut turn);
            }
            retain_requests(previous_turn, &mut turn);
        }
        turns.push(turn);
    }
    fresh["turns"] = Value::Array(turns);
    if retain_history {
        fresh["historyCursor"] = previous["historyCursor"].take();
    }
    (fresh, Ok(()))
}
