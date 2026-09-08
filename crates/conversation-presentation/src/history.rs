//! History selection policy. Plans reference existing values; bodies stay in
//! their native owner and repeated native turn IDs retain their occurrences.
use crate::{array, text};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Previous,
    Incoming,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub source: Source,
    pub index: usize,
}

/// Existing live values win overlap. Items within an older page deduplicate;
/// repeated turns within that page remain separate native occurrences.
pub fn prepend(previous: &[Value], incoming: &[Value], items: bool) -> Vec<Selection> {
    let mut known: HashSet<&str> = previous.iter().map(|value| text(value, "id")).collect();
    let mut selection = Vec::with_capacity(previous.len() + incoming.len());
    for (index, value) in incoming.iter().enumerate() {
        let id = text(value, "id");
        let include = if items {
            known.insert(id)
        } else {
            !known.contains(id)
        };
        if include {
            selection.push(Selection {
                source: Source::Incoming,
                index,
            });
        }
    }
    selection.extend((0..previous.len()).map(|index| Selection {
        source: Source::Previous,
        index,
    }));
    selection
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSelection {
    #[serde(flatten)]
    pub value: Selection,
    pub deferred: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSelection {
    #[serde(flatten)]
    pub value: Selection,
    pub previous_turn: Option<usize>,
    pub items: Option<Vec<ItemSelection>>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshPlan {
    pub turns: Vec<TurnSelection>,
    pub preserve_cursor: bool,
}

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
fn items(previous: &Value, incoming: &Value) -> Option<Vec<ItemSelection>> {
    let old = array(&previous["items"]);
    let new = array(&incoming["items"]);
    if new.is_empty() {
        return has_more(incoming).then(|| {
            old.iter()
                .enumerate()
                .map(|(index, item)| ItemSelection {
                    value: Selection {
                        source: Source::Previous,
                        index,
                    },
                    deferred: deferred(previous, item),
                })
                .collect()
        });
    }
    let boundary = old.iter().position(|item| item["id"] == new[0]["id"])?;
    let mut selection = Vec::with_capacity(boundary + new.len());
    selection.extend(
        old[..boundary]
            .iter()
            .enumerate()
            .map(|(index, item)| ItemSelection {
                value: Selection {
                    source: Source::Previous,
                    index,
                },
                deferred: deferred(previous, item),
            }),
    );
    let mut consumed = vec![false; old.len() - boundary];
    for (index, item) in new.iter().enumerate() {
        // A summary must not discard a detail already fetched by the native
        // owner. Match each old occurrence once, including duplicate item IDs.
        let matched = old[boundary..]
            .iter()
            .enumerate()
            .find(|(offset, old_item)| !consumed[*offset] && old_item["id"] == item["id"])
            .map(|(offset, _)| offset);
        if let Some(offset) = matched {
            consumed[offset] = true;
        }
        if let Some(offset) = matched.filter(|offset| {
            deferred(incoming, item) && !deferred(previous, &old[boundary + offset])
        }) {
            selection.push(ItemSelection {
                value: Selection {
                    source: Source::Previous,
                    index: boundary + offset,
                },
                deferred: false,
            });
        } else {
            selection.push(ItemSelection {
                value: Selection {
                    source: Source::Incoming,
                    index,
                },
                deferred: deferred(incoming, item),
            });
        }
    }
    Some(selection)
}

pub fn refresh(previous: &Value, incoming: &Value) -> RefreshPlan {
    let old = array(&previous["turns"]);
    let new = array(&incoming["turns"]);
    let boundary =
        if previous.get("historyCursor").is_some() && incoming.get("historyCursor").is_some() {
            new.first()
                .and_then(|first| old.iter().position(|turn| turn["id"] == first["id"]))
        } else {
            None
        };
    let mut turns = Vec::with_capacity(boundary.unwrap_or(0) + new.len());
    if let Some(boundary) = boundary {
        turns.extend((0..boundary).map(|index| TurnSelection {
            value: Selection {
                source: Source::Previous,
                index,
            },
            previous_turn: None,
            items: None,
        }));
        let mut consumed = vec![false; old.len() - boundary];
        for (index, turn) in new.iter().enumerate() {
            let previous_turn = old[boundary..]
                .iter()
                .enumerate()
                .find(|(offset, previous)| !consumed[*offset] && previous["id"] == turn["id"])
                .map(|(offset, _)| {
                    consumed[offset] = true;
                    boundary + offset
                });
            turns.push(TurnSelection {
                value: Selection {
                    source: Source::Incoming,
                    index,
                },
                items: previous_turn.and_then(|previous_index| items(&old[previous_index], turn)),
                previous_turn,
            });
        }
    } else {
        turns.extend((0..new.len()).map(|index| TurnSelection {
            value: Selection {
                source: Source::Incoming,
                index,
            },
            previous_turn: None,
            items: None,
        }));
    }
    RefreshPlan {
        turns,
        preserve_cursor: boundary.is_some(),
    }
}

/// Prepending items applies the same live-value and detail-selection policy as
/// refresh; deferred IDs are derived only from the selected source occurrences.
pub fn prepend_items(previous: &Value, incoming: &Value) -> Vec<ItemSelection> {
    prepend(array(&previous["items"]), array(&incoming["items"]), true)
        .into_iter()
        .map(|value| {
            let turn = match value.source {
                Source::Previous => previous,
                Source::Incoming => incoming,
            };
            let deferred = deferred(turn, &turn["items"][value.index]);
            ItemSelection { value, deferred }
        })
        .collect()
}

/// Check the cursor before changing owned state; delayed pages cannot overwrite live content.
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
            let values = older["items"]
                .as_array_mut()
                .ok_or("履歴の項目がありません")?;
            if values.iter().any(|item| text(item, "id").is_empty()) {
                return Err("履歴の項目IDがありません".into());
            }
            let selection = prepend_items(turn, older);
            let mut items = Vec::with_capacity(selection.len());
            let mut deferred = Vec::new();
            for selected in selection {
                let source = match selected.value.source {
                    Source::Previous => &mut *turn,
                    Source::Incoming => &mut *older,
                };
                let item = source["items"][selected.value.index].take();
                if selected.deferred {
                    deferred.push(item["id"].clone());
                }
                items.push(item);
            }
            turn["items"] = Value::Array(items);
            turn["deferredItemIds"] = Value::Array(deferred);
            turn["itemsHasMore"] = json!(has_more(older));
            turn["itemsNextCursor"] = older["itemsNextCursor"].take();
            if !older["openingUserMessage"].is_null() {
                turn["openingUserMessage"] = older["openingUserMessage"].take();
            }
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
            let selection = prepend(
                array(&previous["turns"]),
                array(&page["thread"]["turns"]),
                false,
            );
            let added = selection.len() - array(&previous["turns"]).len();
            let turns = selection
                .into_iter()
                .map(|selected| {
                    let source = match selected.source {
                        Source::Previous => &mut previous,
                        Source::Incoming => &mut page["thread"],
                    };
                    source["turns"][selected.index].take()
                })
                .collect();
            previous["turns"] = Value::Array(turns);
            previous["historyCursor"] = page["thread"]["historyCursor"].take();
            Ok(added)
        }
    })();
    (previous, result)
}

pub fn merge_refresh(mut previous: Value, mut fresh: Value) -> (Value, Result<(), String>) {
    let result = (|| {
        if fresh["id"] != previous["id"] || !fresh["turns"].is_array() {
            return Err("更新された履歴が不正です".into());
        }
        let plan = refresh(&previous, &fresh);
        let mut turns = Vec::with_capacity(plan.turns.len());
        for selected in plan.turns {
            let mut turn = match selected.value.source {
                Source::Previous => previous["turns"][selected.value.index].take(),
                Source::Incoming => fresh["turns"][selected.value.index].take(),
            };
            if let Some(selection) = selected.items {
                let old_turn = &mut previous["turns"][selected.previous_turn.unwrap()];
                let mut items = Vec::with_capacity(selection.len());
                let mut deferred = Vec::new();
                for selected in selection {
                    let source = match selected.value.source {
                        Source::Previous => &mut *old_turn,
                        Source::Incoming => &mut turn,
                    };
                    let item = source["items"][selected.value.index].take();
                    if selected.deferred {
                        deferred.push(item["id"].clone());
                    }
                    items.push(item);
                }
                turn["items"] = Value::Array(items);
                turn["deferredItemIds"] = Value::Array(deferred);
                turn["itemsHasMore"] = json!(has_more(old_turn));
                turn["itemsNextCursor"] = old_turn["itemsNextCursor"].take();
                if turn["openingUserMessage"].is_null() && !old_turn["openingUserMessage"].is_null()
                {
                    turn["openingUserMessage"] = old_turn["openingUserMessage"].take();
                }
            }
            turns.push(turn);
        }
        fresh["turns"] = Value::Array(turns);
        if plan.preserve_cursor {
            fresh["historyCursor"] = previous["historyCursor"].take();
        }
        previous = fresh;
        Ok(())
    })();
    (previous, result)
}
