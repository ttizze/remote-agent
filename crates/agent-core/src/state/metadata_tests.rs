use super::*;
use crate::session::{ProviderKind, SessionRef};

fn id(provider: ProviderKind, name: &str) -> SessionRef {
    SessionRef::new(provider, name.into()).unwrap()
}

#[test]
fn batch_metadata_keeps_first_rows_provider_identity_and_absent_results() {
    let codex = id(ProviderKind::Codex, "same");
    let claude = id(ProviderKind::Claude, "same");
    let read = id(ProviderKind::Codex, "read");
    let missing = id(ProviderKind::Codex, "missing");
    let make = |id: Option<SessionRef>, name: &str, updated_at| Thread {
        id,
        name: Some(name.into()),
        updated_at: Some(updated_at as f64),
        ..Default::default()
    };
    let conversations = [(read.clone(), Arc::new(make(None, "Read conversation", 1)))].into();
    let rows = [
        make(None, "Anonymous", 100),
        make(Some(codex.clone()), "First row", 1),
        make(Some(claude.clone()), "Other provider", 2),
        make(Some(codex.clone()), "Later row", 99),
        make(Some(read.clone()), "Listed conversation", 100),
    ];
    let requested = [&codex, &claude, &read, &missing, &codex];
    let indexed = index_missing_metadata(&conversations, rows.iter(), requested.into_iter());
    assert_eq!(indexed.len(), 3);
    assert!(!indexed.contains_key(&read));
    assert!(std::ptr::eq(indexed[&codex].unwrap(), &rows[1]));
    assert!(std::ptr::eq(indexed[&claude].unwrap(), &rows[2]));
    assert_eq!(indexed[&missing], None);
    assert_eq!(
        conversations[&read].name.as_deref(),
        Some("Read conversation")
    );
}

#[test]
fn batch_metadata_handles_empty_sources_and_requests_without_inventing_rows() {
    let requested = id(ProviderKind::Codex, "requested");
    let unrelated = Thread {
        id: Some(id(ProviderKind::Codex, "other")),
        ..Default::default()
    };
    let conversations = BTreeMap::new();
    for rows in [vec![], vec![unrelated]] {
        let indexed = index_missing_metadata(&conversations, rows.iter(), [&requested].into_iter());
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[&requested], None);
        assert!(index_missing_metadata(&conversations, rows.iter(), std::iter::empty()).is_empty());
    }
}
