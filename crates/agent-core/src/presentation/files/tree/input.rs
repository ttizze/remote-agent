use super::*;
use std::collections::HashSet;

/// Native input produces one shared state action and any resulting UI operation.
pub(super) struct FileTreeInput {
    pub(super) actions: Vec<FileTreeAction>,
    pub(super) menu_path: Option<String>,
}

pub(super) fn focused_path(
    rows: &[FileTreeRow],
    focused: Option<&str>,
    query: &str,
) -> Option<String> {
    let query = desktop_query(query);
    if !query.is_empty() {
        return focused
            .and_then(|focused| {
                rows.iter().find(|row| {
                    row.path == focused && matches_query_path(&row.path, row.kind, &query)
                })
            })
            .or_else(|| {
                rows.iter()
                    .find(|row| matches_query_path(&row.path, row.kind, &query))
            })
            .map(|row| row.path.clone());
    }
    rows.iter()
        .find(|row| Some(row.path.as_str()) == focused)
        .or_else(|| {
            rows.iter()
                .rev()
                .find(|row| focused.is_some_and(|path| is_descendant(path, &row.path)))
        })
        .or_else(|| rows.first())
        .map(|row| row.path.clone())
}
pub(super) fn desktop_query(query: &str) -> String {
    query.trim().replace('\\', "/").to_lowercase()
}
pub(super) fn matches_query_path(path: &str, kind: EntryKind, query: &str) -> bool {
    let path = path.to_lowercase();
    path.contains(query)
        || kind == EntryKind::Directory
            && query
                .strip_suffix('/')
                .is_some_and(|query| path.ends_with(query))
}

fn is_descendant(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

impl FileTreeView {
    pub fn is_search_match(&self, row: &FileTreeRow) -> bool {
        let query = desktop_query(&self.query);
        !query.is_empty() && matches_query_path(&row.path, row.kind, &query)
    }
    /// A compressed directory chain shares a row with its terminal directory.
    pub fn index_for_path(&self, path: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.path == path)
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| is_descendant(&row.path, path))
            })
    }
    pub(super) fn desktop_keyboard_input(
        &self,
        key: &str,
        additive: bool,
        shift: bool,
        anchor: Option<&str>,
    ) -> Option<FileTreeInput> {
        if !self.query.trim().is_empty() {
            return self.search_input(key);
        }
        let focused = self.focused_path.as_deref()?;
        let index = self.rows.iter().position(|row| row.path == focused)?;
        let row = &self.rows[index];
        let next = match key {
            "down" => (index + 1).min(self.rows.len() - 1),
            "up" => index.saturating_sub(1),
            "home" => 0,
            "end" => self.rows.len() - 1,
            _ => index,
        };
        let mut menu_path = None;
        let action = match key {
            "down" | "up" if shift => {
                let mut paths = self.selected_paths.clone();
                if next != index {
                    let next_path = &self.rows[next].path;
                    if paths.contains(&row.path) && paths.contains(next_path) {
                        paths.retain(|path| path != &row.path);
                    } else if !paths.contains(next_path) {
                        paths.push(next_path.clone());
                    }
                }
                FileTreeAction::SelectPaths {
                    paths,
                    anchor: anchor.map(str::to_owned).or_else(|| Some(row.path.clone())),
                    focused: Some(self.rows[next].path.clone()),
                }
            }
            "down" | "up" | "home" | "end" => FileTreeAction::Focus {
                path: self.rows[next].path.clone(),
            },
            "right" if row.kind == EntryKind::Directory && !row.expanded => {
                FileTreeAction::Toggle {
                    path: row.path.clone(),
                }
            }
            "right" => FileTreeAction::Focus {
                path: self.rows[(index + 1).min(self.rows.len() - 1)].path.clone(),
            },
            "left" if row.kind == EntryKind::Directory && row.expanded => FileTreeAction::Toggle {
                path: row.path.clone(),
            },
            "left" => {
                let parent = self.rows[..index]
                    .iter()
                    .rev()
                    .find(|parent| is_descendant(&row.path, &parent.path))?;
                FileTreeAction::Focus {
                    path: parent.path.clone(),
                }
            }
            "a" if additive => {
                let paths: Vec<_> = self.rows.iter().map(|row| row.path.clone()).collect();
                FileTreeAction::SelectPaths {
                    paths,
                    anchor: self
                        .focused_path
                        .clone()
                        .or_else(|| anchor.map(str::to_owned)),
                    focused: self.focused_path.clone(),
                }
            }
            "enter" | "space" => {
                if key == "space" && additive {
                    FileTreeAction::Select {
                        path: focused.into(),
                        additive: true,
                        range: false,
                    }
                } else {
                    FileTreeAction::Click {
                        path: focused.into(),
                        additive,
                        range: shift,
                    }
                }
            }
            "contextmenu" => {
                menu_path = Some(focused.into());
                FileTreeAction::Focus {
                    path: focused.into(),
                }
            }
            _ => return None,
        };
        Some(FileTreeInput {
            actions: vec![action],
            menu_path,
        })
    }
    fn search_input(&self, key: &str) -> Option<FileTreeInput> {
        let action = match key {
            "escape" => FileTreeAction::Query {
                text: String::new(),
            },
            "enter" => {
                let path = self.focused_path.clone()?;
                return Some(FileTreeInput {
                    actions: vec![
                        FileTreeAction::Select {
                            path,
                            additive: false,
                            range: false,
                        },
                        FileTreeAction::Query {
                            text: String::new(),
                        },
                    ],
                    menu_path: None,
                });
            }
            "up" | "down" => {
                let query = desktop_query(&self.query);
                let matches: Vec<_> = self
                    .rows
                    .iter()
                    .filter(|row| matches_query_path(&row.path, row.kind, &query))
                    .collect();
                if matches.is_empty() {
                    return None;
                }
                let index = matches
                    .iter()
                    .position(|row| Some(&row.path) == self.focused_path.as_ref())
                    .unwrap_or_default();
                let next = if key == "down" {
                    (index + 1).min(matches.len() - 1)
                } else {
                    index.saturating_sub(1)
                };
                FileTreeAction::Focus {
                    path: matches[next].path.clone(),
                }
            }
            _ => return None,
        };
        Some(FileTreeInput {
            actions: vec![action],
            menu_path: None,
        })
    }
}

impl FileTreeView {
    /// Pierre suppresses dragging during search and drags the whole selection
    /// only when the grabbed row already belongs to it.
    pub fn drag_paths(&self, path: &str) -> Vec<String> {
        if !self.query.trim().is_empty() {
            Vec::new()
        } else if self.selected_paths.iter().any(|selected| selected == path) {
            self.selected_paths.clone()
        } else {
            vec![path.into()]
        }
    }
    pub fn handles_desktop_key(&self, key: &str, additive: bool, shift: bool) -> bool {
        self.desktop_keyboard_input(key, additive, shift, None)
            .is_some()
    }
}

pub(super) fn selection(
    rows: &[FileTreeRow],
    current: &[String],
    anchor: Option<&str>,
    path: &str,
    additive: bool,
    range: bool,
) -> (Vec<String>, Option<String>) {
    let mut selected = if additive {
        current.to_vec()
    } else {
        Vec::new()
    };
    if range
        && let Some(anchor) = anchor
        && let (Some(first), Some(last)) = (
            rows.iter().position(|row| row.path == anchor),
            rows.iter().position(|row| row.path == path),
        )
    {
        let mut seen: HashSet<_> = if additive {
            current.iter().map(String::as_str).collect()
        } else {
            HashSet::new()
        };
        for row in &rows[first.min(last)..=first.max(last)] {
            if seen.insert(row.path.as_str()) {
                selected.push(row.path.clone());
            }
        }
        return (selected, Some(anchor.into()));
    }
    if let Some(index) = selected.iter().position(|selected| selected == path) {
        if !range {
            selected.remove(index);
        }
    } else {
        selected.push(path.into());
    }
    (selected, Some(path.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_focus_keeps_only_matching_visible_paths_and_otherwise_uses_the_first_match() {
        let rows: Vec<_> = [
            ("src/First.rs", EntryKind::File),
            ("src/子", EntryKind::Directory),
            ("src/İ.rs", EntryKind::File),
            ("src/Second.rs", EntryKind::File),
        ]
        .into_iter()
        .map(|(path, kind)| FileTreeRow {
            path: path.into(),
            label: path.into(),
            kind,
            depth: 1,
            expanded: false,
            selected: false,
            ignored: false,
            loading: false,
        })
        .collect();
        for (focused, query, expected) in [
            (None, "rs", Some("src/First.rs")),
            (Some("src/Second.rs"), "rs", Some("src/Second.rs")),
            (Some("src/子"), "rs", Some("src/First.rs")),
            (Some("src/second.rs"), "rs", Some("src/First.rs")),
            (Some("src/missing.rs"), "rs", Some("src/First.rs")),
            (Some("src/İ.rs"), "  I\u{307}.RS  ", Some("src/İ.rs")),
            (Some("src/Second.rs"), "子/", Some("src/子")),
            (Some("src/Second.rs"), " SRC\\ ", Some("src/Second.rs")),
            (None, "missing", None),
            (Some("src/Second.rs"), "missing", None),
            (Some("src/Second.rs"), "", Some("src/Second.rs")),
            (None, "", Some("src/First.rs")),
        ] {
            assert_eq!(
                focused_path(&rows, focused, query).as_deref(),
                expected,
                "{focused:?} {query:?}"
            );
        }
        assert_eq!(focused_path(&[], Some("src/First.rs"), "rs"), None);
    }

    #[test]
    fn queries_match_literal_paths_and_a_directorys_virtual_trailing_slash() {
        for (path, kind, query, expected) in [
            ("src/MAIN.rs", EntryKind::File, "main.rs", true),
            ("src/子", EntryKind::Directory, "子/", true),
            ("src/子", EntryKind::File, "子/", false),
            ("src/子", EntryKind::Directory, "src/子/", true),
            ("src/子", EntryKind::Directory, "子//", false),
            ("src/子", EntryKind::File, "src/", true),
            ("root", EntryKind::File, "/", false),
            ("root", EntryKind::Directory, "/", true),
            ("root", EntryKind::Directory, "", true),
            ("İ", EntryKind::Directory, "i\u{307}/", true),
        ] {
            assert_eq!(
                matches_query_path(path, kind, query),
                expected,
                "{path:?} {query:?}"
            );
        }
    }

    #[test]
    fn ancestors_require_an_actual_path_boundary() {
        for (path, parent, expected) in [
            ("src/main.rs", "src", true),
            ("src/nested/file", "src", true),
            ("src2/main.rs", "src", false),
            ("src", "src", false),
            ("日本語/子", "日本語", true),
            ("日本語二/子", "日本語", false),
            ("file", "", false),
        ] {
            assert_eq!(is_descendant(path, parent), expected, "{path:?} {parent:?}");
        }
    }
}
