use super::*;
use std::path::PathBuf;

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum Route {
    Stay,
    Away,
    RoundTrip,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct File {
    revision: String,
    text: String,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    opened: Option<File>,
    next_save: Option<File>,
    editor_text: String,
    can_save: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Trace {
    route: Route,
    later: Option<String>,
    captured: File,
    before_reply: Observation,
    after_reply: Observation,
    back: Observation,
}

fn observe(snapshot: &Snapshot) -> Observation {
    let pane = snapshot.panes.primary();
    let editor = pane.file_editor(&snapshot.files.file_drafts, PATH);
    let next = (SaveFile { path: PATH.into() }).capture(snapshot, "");
    Observation {
        opened: pane.workspace.file.as_ref().map(|file| File {
            revision: file.revision.clone(),
            text: file.text.clone(),
        }),
        next_save: next.ok().map(|draft| File {
            revision: draft.revision.clone(),
            text: draft.text.clone(),
        }),
        editor_text: editor.text,
        can_save: editor.can_save,
    }
}

fn replay(trace: &Trace) {
    let mut snapshot = Snapshot::default();
    select(&mut snapshot, "", "first");
    load(&mut snapshot, "", content(PATH, "old-revision", "OLD"));
    edit(&mut snapshot, "", "NEW");
    let operation = SaveFile { path: PATH.into() };
    let submitted = operation.capture(&snapshot, "").unwrap();
    assert_eq!(submitted.text, trace.captured.text, "{trace:?}");
    assert_eq!(submitted.revision, trace.captured.revision, "{trace:?}");
    let saved = content(PATH, "saved-revision", &submitted.text);
    if let Some(text) = &trace.later {
        edit(&mut snapshot, "", text);
    }
    if trace.route != Route::Stay {
        select(&mut snapshot, "", "second");
    }
    if trace.route == Route::RoundTrip {
        select(&mut snapshot, "", "first");
    }
    assert_eq!(observe(&snapshot), trace.before_reply, "{trace:?}");
    operation
        .complete(
            &mut snapshot,
            "",
            (submitted, saved),
            trace.route == Route::Stay,
        )
        .unwrap();
    assert_eq!(observe(&snapshot), trace.after_reply, "{trace:?}");
    if trace.route == Route::Away {
        select(&mut snapshot, "", "first");
    }
    assert_eq!(observe(&snapshot), trace.back, "{trace:?}");
}

#[test]
#[ignore = "requires Lean-generated workspace.jsonl"]
fn lean_core_workspace_conformance() {
    let directory = std::env::var_os("AGENT_CORE_CASES").expect("missing AGENT_CORE_CASES");
    let input = std::fs::read_to_string(PathBuf::from(directory).join("workspace.jsonl"))
        .expect("read workspace.jsonl");
    assert_eq!(input.lines().count(), 15, "incomplete workspace corpus");
    for line in input.lines() {
        replay(&serde_json::from_str(line).expect("invalid Lean workspace trace"));
    }
}
