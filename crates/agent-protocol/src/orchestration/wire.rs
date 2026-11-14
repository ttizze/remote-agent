//! T3's ordinary wire projection. Full bodies stay in the Host; live events,
//! replay, snapshots and history pages all use this same idempotent projection.
use super::{projection::ThreadProjection, *};
use std::sync::{Arc, LazyLock};

const DETAIL_BYTES: usize = 32_768;
const DYNAMIC_BYTES: usize = 16_384;
const TRUNCATION: &str = "\n… output truncated for transport";

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub(TRUNCATION.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION}", &text[..end])
}

fn command_failed(text: &str) -> bool {
    static FAILURE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"(?i)file not found|no files found|enoent|no such file|commandnotfoundexception|",
            r"command not found|is not recognized as the name of a cmdlet|",
            r"a parameter cannot be found that matches parameter name|",
            r"exit(?:ed)? with exit code\s+[1-9]\d*|exit code\s*[:\s]\s*[1-9]\d*\b"
        ))
        .unwrap()
    });
    let mut units = 0;
    let mut end = 0;
    for ch in text.chars() {
        units += ch.len_utf16();
        if units > DETAIL_BYTES {
            break;
        }
        end += ch.len_utf8();
    }
    let text = &text[..end];
    let lower = text.to_ascii_lowercase();
    FAILURE.is_match(text)
        || lower.contains("cannot find path") && lower.contains("because it does not exist")
        || lower.contains("is not recognized") && lower.contains("the term '")
}

fn summarize(value: &Value) -> Value {
    let json = serde_json::to_string(value).expect("JSON values serialize");
    if json.len() <= DYNAMIC_BYTES {
        return value.clone();
    }
    let text = value.as_str().unwrap_or(&json).trim_start();
    let mut first = String::new();
    let mut units = 0;
    let mut space = false;
    let mut truncated = false;
    for ch in text.chars().take_while(|ch| *ch != '\n') {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        let needed = ch.len_utf16() + usize::from(space && !first.is_empty());
        if units + needed > 160 {
            truncated = true;
            break;
        }
        if space && !first.is_empty() {
            first.push(' ');
            units += 1;
        }
        first.push(ch);
        units += ch.len_utf16();
        space = false;
    }
    if truncated {
        while first.encode_utf16().count() > 159 {
            first.pop();
        }
        first = first.trim_end().to_owned();
        first.push('…');
    } else if first.is_empty() {
        first = "Large tool output".into();
    }
    serde_json::json!({"summary":first,"truncated":true})
}

fn has_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        _ => true,
    }
}

fn cap_files(files: &mut Vec<CheckpointFileSummary>, total: &mut Option<CheckpointFilesTotal>) {
    if files.len() <= 200 || total.is_some() {
        return;
    }
    *total = Some(CheckpointFilesTotal {
        count: files.len(),
        additions: files.iter().map(|file| file.additions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
    });
    files.truncate(200);
}

pub fn item(item: &TurnItem) -> TurnItem {
    let mut projected = item.clone();
    match &mut projected.body {
        TurnItemBody::CommandExecution {
            output,
            exit_code,
            output_omitted,
            output_indicates_failure,
            ..
        } => {
            let failed = exit_code.map_or_else(
                || {
                    *output_indicates_failure == Some(true)
                        || output.as_deref().is_some_and(command_failed)
                },
                |code| code != 0,
            );
            if failed {
                *output_indicates_failure = Some(true);
            }
            if output
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
            {
                *output_omitted = Some(true);
            }
            *output = None;
        }
        TurnItemBody::FileChange { diff_str, .. } => {
            *diff_str = diff_str
                .as_ref()
                .filter(|text| item.status == TurnItemStatus::Failed && !text.trim().is_empty())
                .map(|text| truncate(text, DETAIL_BYTES));
        }
        TurnItemBody::Subagent {
            prompt,
            progress,
            result,
            ..
        } => {
            *prompt = truncate(prompt, DETAIL_BYTES);
            *progress = progress.as_deref().map(|text| truncate(text, DETAIL_BYTES));
            *result = result.as_deref().map(|text| truncate(text, DETAIL_BYTES));
        }
        TurnItemBody::DynamicTool {
            input,
            output,
            output_omitted,
            ..
        } => {
            *input = summarize(input);
            if output.as_ref().is_some_and(has_value) {
                *output_omitted = Some(true);
            }
            *output = output.as_ref().and_then(crate::tool_output::compact);
        }
        TurnItemBody::Checkpoint {
            files, files_total, ..
        } => cap_files(files, files_total),
        TurnItemBody::Handoff { summary, .. } => *summary = None,
        _ => {}
    }
    projected
}

fn handoff(handoff: &mut ContextHandoff) {
    handoff.history = None;
    handoff.delivery = None;
}

pub fn event(event: &Event) -> Event {
    let mut projected = event.clone();
    match &mut projected.change {
        Change::TurnItem(value) => *value = item(value),
        Change::Checkpoint(value) => cap_files(&mut value.files, &mut value.files_total),
        Change::ContextHandoff(value) => handoff(value),
        _ => {}
    }
    projected
}

pub fn projection(projection: &mut ThreadProjection) {
    let mut projected = BTreeMap::new();
    let mut project = |value: &Arc<TurnItem>| {
        projected
            .entry((value.thread_id.clone(), value.id.clone()))
            .or_insert_with(|| Arc::new(item(value)))
            .clone()
    };
    for value in &mut projection.turn_items {
        *value = project(value);
    }
    for row in &mut projection.visible_turn_items {
        row.item = project(&row.item);
    }
    for value in &mut projection.checkpoints {
        cap_files(&mut value.files, &mut value.files_total);
    }
    for value in &mut projection.context_handoffs {
        handoff(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn_item(body: Value) -> TurnItem {
        let mut value = json!({"id":"item","threadId":"thread","runId":null,"nodeId":null,
            "providerThreadId":null,"providerTurnId":null,"nativeItemRef":null,"parentItemId":null,
            "ordinal":1,"status":"completed","title":null,"startedAt":null,"completedAt":null,"updatedAt":"2026-11-14T00:00:00.000Z"});
        value
            .as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn command_output_is_on_demand_and_explicit_exit_codes_override_text_heuristics() {
        for (exit, expected) in [(None, true), (Some(0), false), (Some(2), true)] {
            let mut body =
                json!({"type":"command_execution","input":"rg ENOENT","output":"file not found"});
            if let Some(exit) = exit {
                body["exitCode"] = exit.into();
            }
            let full = turn_item(body);
            let wire = item(&full);
            let value = serde_json::to_value(&wire).unwrap();
            assert!(value.get("output").is_none());
            assert_eq!(value["outputOmitted"], true);
            assert_eq!(value["outputIndicatesFailure"] == true, expected);
            assert_eq!(item(&wire), wire);
            assert!(serde_json::to_value(&full).unwrap().get("output").is_some());
        }
        for output in ["", " \n\t"] {
            let wire = item(&turn_item(
                json!({"type":"command_execution","input":"true","output":output}),
            ));
            assert!(
                serde_json::to_value(wire)
                    .unwrap()
                    .get("outputOmitted")
                    .is_none()
            );
        }
    }

    #[test]
    fn command_error_detection_matches_t3s_cross_platform_cases() {
        for output in [
            "ENOENT",
            "No such file",
            "exit code: 12",
            "exited with exit code 4",
            "Cannot find path 'x' because it does not exist",
            "The term 'rg' is not recognized",
        ] {
            assert!(command_failed(output), "{output}");
        }
        for output in [
            "exit code 0",
            "Text not found in this search",
            "exit code: 01",
        ] {
            assert!(!command_failed(output), "{output}");
        }
        assert!(command_failed(&format!("{}ENOENT", "あ".repeat(20_000))));
        assert!(!command_failed(&format!("{}ENOENT", "あ".repeat(32_768))));
    }

    #[test]
    fn failed_diffs_are_bounded_utf8_and_successful_diffs_do_not_enter_the_stream() {
        let full = turn_item(
            json!({"type":"file_change","fileName":"src/lib.rs","diffStr":"あ".repeat(40_000)}),
        );
        assert!(
            serde_json::to_value(item(&full))
                .unwrap()
                .get("diffStr")
                .is_none()
        );
        let failed = TurnItem {
            status: TurnItemStatus::Failed,
            ..full
        };
        let projected = item(&failed);
        let value = serde_json::to_value(&projected).unwrap();
        let diff = value["diffStr"].as_str().unwrap();
        assert!(diff.len() <= DETAIL_BYTES);
        assert!(diff.ends_with(TRUNCATION));
        assert!(!diff.contains('\u{fffd}'));
        assert_eq!(item(&projected), projected);
    }

    #[test]
    fn dynamic_results_keep_metadata_without_transporting_tool_payloads() {
        let full = turn_item(
            json!({"type":"dynamic_tool","toolName":"run","input":{"text":"入力".repeat(20_000)},
            "output":{"content":[{"type":"text","text":"raw log"},{"type":"image","mimeType":"image/png","data":"image bytes"}],
                "structuredContent":{"threadId":"child","taskId":"task","sentMessage":"hello","payload":"large data"}}}),
        );
        let wire = item(&full);
        let value = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            value["output"],
            json!({"threadId":"child","taskId":"task","sentMessage":"hello"})
        );
        assert_eq!(value["outputOmitted"], true);
        assert_eq!(value["input"]["truncated"], true);
        assert!(
            value["input"]["summary"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .count()
                <= 160
        );
        assert_eq!(item(&wire), wire);
        assert_eq!(
            serde_json::to_value(&full).unwrap()["output"]["content"][1]["data"],
            "image bytes"
        );
    }

    #[test]
    fn checkpoint_cards_keep_totals_while_capping_the_file_list() {
        let files: Vec<_>=(0..250).map(|index|json!({"path":format!("{index}.rs"),"kind":"modified","additions":2,"deletions":1})).collect();
        let full = turn_item(
            json!({"type":"checkpoint","checkpointId":"checkpoint","scopeId":"scope","files":files}),
        );
        let wire = item(&full);
        let value = serde_json::to_value(&wire).unwrap();
        assert_eq!(value["files"].as_array().unwrap().len(), 200);
        assert_eq!(
            value["filesTotal"],
            json!({"count":250,"additions":500,"deletions":250})
        );
        assert_eq!(item(&wire), wire);
    }
}
