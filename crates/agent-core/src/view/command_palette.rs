//! Search and navigation rules for the command palette.
//!
//! The desktop and native keyboard surfaces render these records. Keeping
//! filtering and keyboard movement here prevents each client from inventing a
//! slightly different command order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CommandPaletteItemKind {
    Action,
    Project,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CommandPaletteItem {
    pub key: String,
    pub kind: CommandPaletteItemKind,
    pub title: String,
    pub detail: Option<String>,
    pub search_terms: Vec<String>,
}

/// Filters palette entries. A leading `>` limits results to actions. Results
/// rank exact, prefix, and substring title matches while retaining stable input
/// order for ties. `matched_threads` permits a content search to surface a
/// thread even when its title does not contain the query.
pub fn filter(
    items: &[CommandPaletteItem],
    query: &str,
    matched_threads: &std::collections::HashSet<String>,
) -> Vec<CommandPaletteItem> {
    let actions_only = query.starts_with('>');
    let normalized = query
        .strip_prefix('>')
        .unwrap_or(query)
        .trim()
        .to_lowercase();
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let mut ranked = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if actions_only && item.kind != CommandPaletteItemKind::Action {
                return None;
            }
            if normalized.is_empty() {
                return (item.kind != CommandPaletteItemKind::Project).then_some((
                    0_u8,
                    index,
                    item.clone(),
                ));
            }
            let title = item.title.to_lowercase();
            let haystack = std::iter::once(title.clone())
                .chain(item.search_terms.iter().map(|term| term.to_lowercase()))
                .collect::<Vec<_>>()
                .join(" ");
            let matched = tokens.iter().all(|token| haystack.contains(token))
                || (item.kind == CommandPaletteItemKind::Thread
                    && matched_threads.contains(&item.key));
            if !matched {
                return None;
            }
            let rank = if title == normalized {
                3
            } else if title.starts_with(&normalized) {
                2
            } else if title.contains(&normalized) {
                1
            } else {
                0
            };
            Some((rank, index, item.clone()))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, index, _)| (std::cmp::Reverse(*rank), *index));
    ranked.into_iter().map(|(_, _, item)| item).collect()
}

/// Moves a highlighted index with wrapping. Empty palettes always return zero.
pub fn next_index(index: usize, direction: i8, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    match direction {
        -1 if index == 0 => count - 1,
        -1 => index - 1,
        1 => (index + 1) % count,
        _ => index.min(count - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn item(key: &str, kind: CommandPaletteItemKind, title: &str) -> CommandPaletteItem {
        CommandPaletteItem {
            key: key.into(),
            kind,
            title: title.into(),
            detail: None,
            search_terms: vec![],
        }
    }

    #[test]
    fn action_prefix_filters_projects_and_threads() {
        let items = vec![
            item("project", CommandPaletteItemKind::Project, "Project"),
            item("thread", CommandPaletteItemKind::Thread, "Thread"),
            item("settings", CommandPaletteItemKind::Action, "Settings"),
        ];
        let mut matches = std::collections::HashSet::new();
        assert_eq!(filter(&items, "", &matches).len(), 2);
        assert_eq!(filter(&items, ">set", &matches)[0].key, "settings");
        matches.insert("thread".into());
        assert_eq!(filter(&items, "unrelated", &matches)[0].key, "thread");
    }

    #[test]
    fn title_matches_rank_before_stable_ties() {
        let items = vec![
            item("contains", CommandPaletteItemKind::Action, "Open settings"),
            item("exact", CommandPaletteItemKind::Action, "Settings"),
            item("prefix", CommandPaletteItemKind::Action, "Settings page"),
        ];
        let keys: Vec<_> = filter(&items, "settings", &Default::default())
            .into_iter()
            .map(|item| item.key)
            .collect();
        assert_eq!(keys, ["exact", "prefix", "contains"]);
    }

    #[test]
    fn search_terms_are_case_insensitive() {
        let mut searchable = item("thread", CommandPaletteItemKind::Thread, "Review");
        searchable.search_terms = vec!["Pull Request".into()];
        assert_eq!(
            filter(&[searchable], "request", &Default::default()).len(),
            1
        );
    }

    #[test]
    fn keyboard_movement_wraps() {
        assert_eq!(next_index(0, -1, 3), 2);
        assert_eq!(next_index(2, 1, 3), 0);
        assert_eq!(next_index(9, 1, 0), 0);
    }

    proptest! {
        #[test]
        fn keyboard_movement_stays_in_bounds(
            index in any::<usize>(),
            count in 1usize..128,
            direction in prop_oneof![Just(-1i8), Just(1i8)],
        ) {
            prop_assert!(next_index(index, direction, count) < count);
        }
    }
}
