use super::*;
use crate::state::operations::{Operation, RenameSession};
use std::sync::Arc;

#[test]
fn rename_commit_matches_javascript_whitespace_and_preserves_interior_text() {
    for (original, value, expected) in [
        ("Original", "Original", ThreadRename::Unchanged),
        (
            "Original",
            " \u{feff}Original\u{3000}",
            ThreadRename::Unchanged,
        ),
        (
            "Original",
            " \u{feff}新しい名前\u{a0}",
            ThreadRename::Name("新しい名前".into()),
        ),
        ("Original", " \u{feff}\u{3000}\r\n", ThreadRename::Empty),
        ("", "", ThreadRename::Empty),
        (" Name ", " Name ", ThreadRename::Name("Name".into())),
        ("Original", "a  b", ThreadRename::Name("a  b".into())),
        (
            "Original",
            "\u{85}name\u{85}",
            ThreadRename::Name("\u{85}name\u{85}".into()),
        ),
    ] {
        assert_eq!(thread_rename(original, value), expected);
    }
}

#[test]
fn rename_boundary_rechecks_name_and_connection_and_does_not_follow_navigation() {
    let session = crate::session::SessionRef::new(
        crate::session::ProviderKind::Codex,
        "rename-original".into(),
    )
    .unwrap();
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        session.clone(),
        Arc::new(crate::models::Thread {
            id: Some(session.clone()),
            name: Some("Original".into()),
            ..Default::default()
        }),
    );
    let request = |name: &str| RenameSession {
        thread_id: session.clone(),
        name: name.into(),
    };
    assert!(request(" \u{feff} ").capture(&snapshot, "").is_err());
    assert_eq!(request(" Original ").capture(&snapshot, "").unwrap(), None);
    assert!(request("Changed").capture(&snapshot, "").is_err());
    snapshot.connection.connected = true;
    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
        "different-draft".into();
    assert_eq!(
        request(" \u{feff}Changed ").capture(&snapshot, "").unwrap(),
        Some("Changed".into())
    );
    assert!(!request("Changed").navigation_bound());
}
