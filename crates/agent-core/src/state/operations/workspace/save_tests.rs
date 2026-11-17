use super::*;
use crate::session::ProviderKind;

mod conformance;

const PATH: &str = "/project/notes.txt";

fn select(snapshot: &mut Snapshot, pane_id: &str, name: &str) {
    let session = crate::session::SessionRef::new(ProviderKind::Codex, name.into()).unwrap();
    crate::state::navigate(
        snapshot,
        pane_id,
        Navigation {
            cwd: "/project".into(),
            draft_key: session.clone().into(),
            thread_id: Some(session),
        },
    );
}

fn content(path: &str, revision: &str, text: &str) -> FileContent {
    FileContent {
        path: path.into(),
        revision: revision.into(),
        text: text.into(),
        size: text.len() as u64,
        truncated: false,
    }
}

fn load(snapshot: &mut Snapshot, pane_id: &str, file: FileContent) {
    ReadFile {
        path: file.path.clone(),
        discard_draft: false,
    }
    .apply(snapshot, pane_id, file);
}

fn edit(snapshot: &mut Snapshot, pane_id: &str, text: &str) {
    let (next, effects) = crate::state::reduce(
        snapshot,
        pane_id,
        Event::from(Intent::SetFileDraft {
            path: PATH.into(),
            text: text.into(),
        }),
    );
    assert!(effects.is_empty());
    *snapshot = next;
}

fn assert_saved(snapshot: &Snapshot, pane_id: &str) {
    let editor = snapshot
        .panes
        .get(pane_id)
        .unwrap()
        .file_editor(&snapshot.files.file_drafts, PATH);
    assert_eq!(editor.text, "NEW");
    assert_eq!(editor.loaded_revision.as_deref(), Some("saved-revision"));
    assert!(!editor.modified);
    assert!(!editor.can_save);
    assert!(editor.conflict_notice.is_none());
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn saved_file_stays_current_after_switching_conversations(#[case] complete_away: bool) {
    let mut snapshot = Snapshot::default();
    select(&mut snapshot, "", "first");
    load(&mut snapshot, "", content(PATH, "old-revision", "OLD"));
    edit(&mut snapshot, "", "NEW");
    let operation = SaveFile { path: PATH.into() };
    let submitted = complete_away.then(|| operation.capture(&snapshot, "").unwrap());
    select(&mut snapshot, "", "second");
    if !complete_away {
        load(&mut snapshot, "", content(PATH, "old-revision", "OLD"));
    }
    let submitted = submitted.unwrap_or_else(|| operation.capture(&snapshot, "").unwrap());
    operation
        .complete(
            &mut snapshot,
            "",
            (submitted, content(PATH, "saved-revision", "NEW")),
            !complete_away,
        )
        .unwrap();
    assert!(!snapshot.files.file_drafts.contains_key(PATH));
    select(&mut snapshot, "", "first");
    assert_saved(&snapshot, "");
    edit(&mut snapshot, "", "NEXT");
    assert_eq!(
        (SaveFile { path: PATH.into() })
            .capture(&snapshot, "")
            .unwrap()
            .revision,
        "saved-revision"
    );
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn saving_updates_open_and_cached_copies_without_changing_other_files(#[case] current: bool) {
    let mut snapshot = Snapshot::default();
    select(&mut snapshot, "", "cached");
    load(&mut snapshot, "", content(PATH, "older-revision", "OLDER"));
    select(&mut snapshot, "", "first");
    load(&mut snapshot, "", content(PATH, "old-revision", "OLD"));
    snapshot.open_pane("other".into(), "/project".into());
    load(&mut snapshot, "other", content(PATH, "old-revision", "OLD"));
    snapshot.open_pane("unrelated".into(), "/project".into());
    load(
        &mut snapshot,
        "unrelated",
        content("/project/other.txt", "unrelated-revision", "UNRELATED"),
    );
    snapshot.open_pane("empty".into(), "/project".into());
    edit(&mut snapshot, "", "NEW");
    let before = snapshot.clone();
    let operation = SaveFile { path: PATH.into() };
    let submitted = operation.capture(&snapshot, "").unwrap();
    operation
        .complete(
            &mut snapshot,
            "",
            (submitted, content(PATH, "saved-revision", "NEW")),
            current,
        )
        .unwrap();
    assert_saved(&snapshot, "");
    assert_saved(&snapshot, "other");
    select(&mut snapshot, "", "cached");
    assert_saved(&snapshot, "");
    assert_eq!(
        snapshot.panes.get("unrelated").unwrap().workspace.file,
        before.panes.get("unrelated").unwrap().workspace.file
    );
    for pane_id in ["unrelated", "empty"] {
        assert!(Arc::ptr_eq(
            snapshot.panes.get(pane_id).unwrap(),
            before.panes.get(pane_id).unwrap()
        ));
    }
    assert_eq!(
        before.panes.primary().workspace.file.as_ref().unwrap().text,
        "OLD"
    );
    assert_eq!(before.files.file_drafts[PATH].text, "NEW");
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn save_completion_preserves_later_edits_and_rebases_the_next_save(#[case] complete_away: bool) {
    let mut snapshot = Snapshot::default();
    select(&mut snapshot, "", "first");
    load(&mut snapshot, "", content(PATH, "old-revision", "OLD"));
    edit(&mut snapshot, "", "NEW");
    let operation = SaveFile { path: PATH.into() };
    let submitted = operation.capture(&snapshot, "").unwrap();
    edit(&mut snapshot, "", "STILL EDITING");
    if complete_away {
        select(&mut snapshot, "", "second");
    }
    operation
        .complete(
            &mut snapshot,
            "",
            (submitted, content(PATH, "saved-revision", "NEW")),
            !complete_away,
        )
        .unwrap();
    select(&mut snapshot, "", "first");
    let editor = snapshot
        .panes
        .primary()
        .file_editor(&snapshot.files.file_drafts, PATH);
    assert_eq!(editor.text, "STILL EDITING");
    assert_eq!(editor.loaded_revision.as_deref(), Some("saved-revision"));
    assert!(editor.can_save);
    assert!(editor.conflict_notice.is_none());
    let operation = SaveFile { path: PATH.into() };
    let submitted = operation.capture(&snapshot, "").unwrap();
    assert_eq!(submitted.revision, "saved-revision");
    assert_eq!(submitted.text, "STILL EDITING");
    operation
        .complete(
            &mut snapshot,
            "",
            (submitted, content(PATH, "next-revision", "STILL EDITING")),
            true,
        )
        .unwrap();
    assert!(!snapshot.files.file_drafts.contains_key(PATH));
    assert!(
        !snapshot
            .panes
            .primary()
            .file_editor(&snapshot.files.file_drafts, PATH)
            .modified
    );
}

#[test]
fn a_save_from_a_closed_pane_updates_the_remaining_file_views() {
    let mut snapshot = Snapshot::default();
    load(&mut snapshot, "", content(PATH, "old-revision", "OLD"));
    snapshot.open_pane("source".into(), "/project".into());
    load(
        &mut snapshot,
        "source",
        content(PATH, "old-revision", "OLD"),
    );
    edit(&mut snapshot, "source", "NEW");
    let operation = SaveFile { path: PATH.into() };
    let submitted = operation.capture(&snapshot, "source").unwrap();
    snapshot.close_pane("source");
    operation
        .complete(
            &mut snapshot,
            "source",
            (submitted, content(PATH, "saved-revision", "NEW")),
            false,
        )
        .unwrap();
    assert!(snapshot.panes.get("source").is_none());
    assert_saved(&snapshot, "");
}
