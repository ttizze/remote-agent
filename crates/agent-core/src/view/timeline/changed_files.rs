//! The changed-files card under an assistant response: a summary, a folder
//! tree with line stats, and which checkpoint each response shows.
use crate::view::collation::numeric_locale_compare;
use crate::view::quantity;
use agent_domain::{
    Checkpoint, CheckpointFile, CheckpointId, CheckpointStatus, MessageId, Role, RunId, State,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DiffStat {
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTreeNode {
    Directory {
        name: String,
        path: String,
        stat: DiffStat,
        children: Vec<DiffTreeNode>,
    },
    File {
        name: String,
        path: String,
        stat: DiffStat,
    },
}

impl DiffTreeNode {
    fn name(&self) -> &str {
        match self {
            Self::Directory { name, .. } | Self::File { name, .. } => name,
        }
    }
}

fn stat_of(file: &CheckpointFile) -> DiffStat {
    DiffStat {
        additions: file.additions,
        deletions: file.deletions,
    }
}

pub fn summarize_diff_stats(files: &[CheckpointFile]) -> DiffStat {
    files
        .iter()
        .fold(DiffStat::default(), |sum, file| DiffStat {
            additions: sum.additions + file.additions,
            deletions: sum.deletions + file.deletions,
        })
}

pub fn has_non_zero_stat(stat: DiffStat) -> bool {
    stat.additions > 0 || stat.deletions > 0
}

#[derive(Default)]
struct MutableDirectory {
    name: String,
    path: String,
    stat: DiffStat,
    directories: Vec<MutableDirectory>,
    files: Vec<DiffTreeNode>,
}

fn compact(node: DiffTreeNode) -> DiffTreeNode {
    let DiffTreeNode::Directory {
        mut name,
        mut path,
        mut stat,
        children,
    } = node
    else {
        return node;
    };
    let mut children: Vec<_> = children.into_iter().map(compact).collect();
    while let [DiffTreeNode::Directory { .. }] = children.as_slice() {
        let Some(DiffTreeNode::Directory {
            name: child_name,
            path: child_path,
            stat: child_stat,
            children: grandchildren,
        }) = children.pop()
        else {
            unreachable!()
        };
        name = format!("{name}/{child_name}");
        path = child_path;
        stat = child_stat;
        children = grandchildren;
    }
    DiffTreeNode::Directory {
        name,
        path,
        stat,
        children,
    }
}

fn into_nodes(directory: MutableDirectory) -> Vec<DiffTreeNode> {
    let mut directories = directory.directories;
    directories.sort_by(|a, b| numeric_locale_compare(&a.name, &b.name));
    let mut files = directory.files;
    files.sort_by(|a, b| numeric_locale_compare(a.name(), b.name()));
    directories
        .into_iter()
        .map(|subdirectory| {
            compact(DiffTreeNode::Directory {
                name: subdirectory.name.clone(),
                path: subdirectory.path.clone(),
                stat: subdirectory.stat,
                children: into_nodes(subdirectory),
            })
        })
        .chain(files)
        .collect()
}

/// Folders before files, each sorted by name; single-folder chains merge.
pub fn build_diff_tree(files: &[CheckpointFile]) -> Vec<DiffTreeNode> {
    let mut root = MutableDirectory::default();
    for file in files {
        let normalized = file.path.replace('\\', "/");
        let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let Some((file_name, parents)) = segments.split_last() else {
            continue;
        };
        let stat = stat_of(file);
        let mut current = &mut root;
        for segment in parents {
            let index = match current
                .directories
                .iter()
                .position(|directory| directory.name == *segment)
            {
                Some(index) => index,
                None => {
                    let path = if current.path.is_empty() {
                        (*segment).to_owned()
                    } else {
                        format!("{}/{segment}", current.path)
                    };
                    current.directories.push(MutableDirectory {
                        name: (*segment).to_owned(),
                        path,
                        ..MutableDirectory::default()
                    });
                    current.directories.len() - 1
                }
            };
            current = &mut current.directories[index];
            current.stat.additions += stat.additions;
            current.stat.deletions += stat.deletions;
        }
        current.files.push(DiffTreeNode::File {
            name: (*file_name).to_owned(),
            path: segments.join("/"),
            stat,
        });
    }
    into_nodes(root)
}

/// Counts as `1.2k`, `15k`, `3m`, `1.5b`.
pub fn format_compact_diff_count(value: u64) -> String {
    let scaled = |divisor: f64, unit: &str| {
        let scaled = value as f64 / divisor;
        if scaled < 10.0 {
            // `toFixed` rounds an exact tie (a quarter) up, not to even.
            let quarters = scaled * 4.0;
            let tenths = if quarters.fract() == 0.0 && quarters % 2.0 == 1.0 {
                format!("{:.1}", (scaled * 10.0).ceil() / 10.0)
            } else {
                format!("{scaled:.1}")
            };
            format!("{}{unit}", tenths.strip_suffix(".0").unwrap_or(&tenths))
        } else {
            format!("{}{unit}", scaled.round())
        }
    };
    match value {
        0..1_000 => value.to_string(),
        1_000..1_000_000 => scaled(1e3, "k"),
        1_000_000..1_000_000_000 => scaled(1e6, "m"),
        _ => scaled(1e9, "b"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffStatLabel {
    pub additions: String,
    pub deletions: String,
    pub accessibility_label: String,
}

pub fn diff_stat_label(stat: DiffStat) -> DiffStatLabel {
    DiffStatLabel {
        additions: format!("+{}", format_compact_diff_count(stat.additions)),
        deletions: format!("-{}", format_compact_diff_count(stat.deletions)),
        accessibility_label: format!("{} additions, {} deletions", stat.additions, stat.deletions),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangedFileRowKind {
    Directory { expanded: bool },
    File,
}

/// One visible row of the folder tree, depth-first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFileRow {
    pub kind: ChangedFileRowKind,
    pub name: String,
    pub path: String,
    pub depth: u32,
    /// Folders without changed lines show none.
    pub stat: Option<DiffStatLabel>,
    /// Files align under folder chevrons when the tree has folders.
    pub leading_spacer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFilesCard {
    pub run: RunId,
    pub title: String,
    pub stat: Option<DiffStatLabel>,
    /// The expand/collapse-all control, present when any path has a folder.
    pub toggle_all_label: Option<String>,
    pub open_diff_label: String,
    pub open_diff_tooltip: String,
    /// The file "Open diff" opens at.
    pub open_diff_path: Option<String>,
    /// Folder overrides apply only while this key is unchanged.
    pub expansion_key: String,
    pub rows: Vec<ChangedFileRow>,
}

fn directory_paths(nodes: &[DiffTreeNode], paths: &mut Vec<String>) {
    for node in nodes {
        if let DiffTreeNode::Directory { path, children, .. } = node {
            paths.push(path.clone());
            directory_paths(children, paths);
        }
    }
}

fn visible_rows(
    nodes: &[DiffTreeNode],
    depth: u32,
    has_directories: bool,
    all_expanded: bool,
    overrides: &BTreeMap<String, bool>,
    rows: &mut Vec<ChangedFileRow>,
) {
    for node in nodes {
        match node {
            DiffTreeNode::Directory {
                name,
                path,
                stat,
                children,
            } => {
                let expanded = overrides.get(path).copied().unwrap_or(all_expanded);
                rows.push(ChangedFileRow {
                    kind: ChangedFileRowKind::Directory { expanded },
                    name: name.clone(),
                    path: path.clone(),
                    depth,
                    stat: has_non_zero_stat(*stat).then(|| diff_stat_label(*stat)),
                    leading_spacer: false,
                });
                if expanded {
                    visible_rows(
                        children,
                        depth + 1,
                        has_directories,
                        all_expanded,
                        overrides,
                        rows,
                    );
                }
            }
            DiffTreeNode::File { name, path, stat } => rows.push(ChangedFileRow {
                kind: ChangedFileRowKind::File,
                name: name.clone(),
                path: path.clone(),
                depth,
                stat: Some(diff_stat_label(*stat)),
                leading_spacer: has_directories || depth > 0,
            }),
        }
    }
}

/// `all_expanded` is the per-run expand-all choice (collapsed until chosen);
/// `overrides` are folders toggled since, by path.
pub fn changed_files_card(
    run: &RunId,
    files: &[CheckpointFile],
    all_expanded: bool,
    overrides: &BTreeMap<String, bool>,
) -> ChangedFilesCard {
    let stat = summarize_diff_stats(files);
    let tree = build_diff_tree(files);
    let mut directories = vec![];
    directory_paths(&tree, &mut directories);
    let mut rows = vec![];
    visible_rows(
        &tree,
        0,
        !directories.is_empty(),
        all_expanded,
        overrides,
        &mut rows,
    );
    ChangedFilesCard {
        run: run.clone(),
        title: quantity(files.len(), "changed file"),
        stat: has_non_zero_stat(stat).then(|| diff_stat_label(stat)),
        toggle_all_label: files
            .iter()
            .any(|file| file.path.contains(['/', '\\']))
            .then(|| {
                if all_expanded {
                    "Collapse all folders"
                } else {
                    "Expand all folders"
                }
                .into()
            }),
        open_diff_label: "Open diff".into(),
        open_diff_tooltip: "Open the full diff".into(),
        open_diff_path: files.first().map(|file| file.path.clone()),
        expansion_key: format!(
            "{}\u{0}{}",
            if all_expanded {
                "expanded"
            } else {
                "collapsed"
            },
            directories.join("\u{0}")
        ),
        rows,
    }
}

/// Flips a folder in `overrides`, starting from the expand-all choice.
pub fn toggle_directory(overrides: &mut BTreeMap<String, bool>, path: &str, all_expanded: bool) {
    let expanded = overrides.get(path).copied().unwrap_or(all_expanded);
    overrides.insert(path.to_owned(), !expanded);
}

/// A checkpoint as the response that ended its run presents it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnDiffSummary {
    pub checkpoint: CheckpointId,
    pub run: RunId,
    pub checkpoint_turn_count: u64,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFile>,
    /// The run's last assistant message.
    pub assistant_message: Option<MessageId>,
}

/// `None` for a checkpoint that belongs to no run.
pub(crate) fn turn_diff_summary(state: &State, checkpoint: &Checkpoint) -> Option<TurnDiffSummary> {
    let run = checkpoint.run.clone()?;
    Some(TurnDiffSummary {
        checkpoint: checkpoint.id.clone(),
        checkpoint_turn_count: checkpoint.run_ordinal,
        status: checkpoint.status,
        files: checkpoint.files.clone(),
        assistant_message: state
            .messages
            .iter()
            .rev()
            .find(|message| message.run.as_ref() == Some(&run) && message.role == Role::Assistant)
            .map(|message| message.id.clone()),
        run,
    })
}

pub fn turn_diff_summaries(state: &State) -> Vec<TurnDiffSummary> {
    state
        .checkpoints
        .iter()
        .filter_map(|checkpoint| turn_diff_summary(state, checkpoint))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantTurnDiff {
    pub message: MessageId,
    pub summary: TurnDiffSummary,
}

/// The summary each assistant message shows; a later checkpoint of the same
/// message replaces an earlier one.
pub fn assistant_turn_diffs(state: &State) -> Vec<AssistantTurnDiff> {
    let mut diffs: Vec<AssistantTurnDiff> = vec![];
    for summary in turn_diff_summaries(state) {
        let Some(message) = summary.assistant_message.clone() else {
            continue;
        };
        match diffs.iter_mut().find(|diff| diff.message == message) {
            Some(diff) => diff.summary = summary,
            None => diffs.push(AssistantTurnDiff { message, summary }),
        }
    }
    diffs
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{Checkpoint, InputIntent, Message, MessageAuthor, Timestamp};

    fn file(path: &str, additions: u64, deletions: u64) -> CheckpointFile {
        CheckpointFile {
            path: path.into(),
            kind: "modified".into(),
            additions,
            deletions,
        }
    }
    fn directory(
        name: &str,
        path: &str,
        stat: (u64, u64),
        children: Vec<DiffTreeNode>,
    ) -> DiffTreeNode {
        DiffTreeNode::Directory {
            name: name.into(),
            path: path.into(),
            stat: DiffStat {
                additions: stat.0,
                deletions: stat.1,
            },
            children,
        }
    }
    fn leaf(name: &str, path: &str, stat: (u64, u64)) -> DiffTreeNode {
        DiffTreeNode::File {
            name: name.into(),
            path: path.into(),
            stat: DiffStat {
                additions: stat.0,
                deletions: stat.1,
            },
        }
    }
    fn visible(files: &[CheckpointFile], all_expanded: bool) -> Vec<String> {
        changed_files_card(
            &RunId::new("turn-1").unwrap(),
            files,
            all_expanded,
            &BTreeMap::new(),
        )
        .rows
        .into_iter()
        .map(|row| row.name)
        .collect()
    }

    #[test]
    fn sums_only_files_with_numeric_additions_deletions() {
        assert_eq!(
            summarize_diff_stats(&[
                file("README.md", 3, 1),
                file("docs/notes.md", 0, 0),
                file("src/index.ts", 5, 2),
            ]),
            DiffStat {
                additions: 8,
                deletions: 3
            }
        );
    }

    #[test]
    fn builds_nested_directory_nodes_with_aggregated_stats() {
        assert_eq!(
            build_diff_tree(&[
                file("src/index.ts", 2, 1),
                file("src/components/Button.tsx", 4, 2),
                file("README.md", 1, 0),
            ]),
            [
                directory(
                    "src",
                    "src",
                    (6, 3),
                    vec![
                        directory(
                            "components",
                            "src/components",
                            (4, 2),
                            vec![leaf("Button.tsx", "src/components/Button.tsx", (4, 2))],
                        ),
                        leaf("index.ts", "src/index.ts", (2, 1)),
                    ],
                ),
                leaf("README.md", "README.md", (1, 0)),
            ]
        );
    }

    #[test]
    fn keeps_zero_valued_file_stats_and_includes_only_their_numeric_contribution() {
        assert_eq!(
            build_diff_tree(&[file("docs/notes.md", 0, 0), file("docs/todo.md", 1, 1)]),
            [directory(
                "docs",
                "docs",
                (1, 1),
                vec![
                    leaf("notes.md", "docs/notes.md", (0, 0)),
                    leaf("todo.md", "docs/todo.md", (1, 1)),
                ],
            )]
        );
    }

    #[test]
    fn normalizes_file_paths_with_windows_separators() {
        assert_eq!(
            build_diff_tree(&[file("apps\\web\\src\\index.ts", 2, 1)]),
            [directory(
                "apps/web/src",
                "apps/web/src",
                (2, 1),
                vec![leaf("index.ts", "apps/web/src/index.ts", (2, 1))],
            )]
        );
    }

    #[test]
    fn compacts_only_single_directory_chains_and_stops_at_branch_points() {
        assert_eq!(
            build_diff_tree(&[
                file("apps/server/src/index.ts", 2, 1),
                file("apps/server/main.ts", 4, 0),
            ]),
            [directory(
                "apps/server",
                "apps/server",
                (6, 1),
                vec![
                    directory(
                        "src",
                        "apps/server/src",
                        (2, 1),
                        vec![leaf("index.ts", "apps/server/src/index.ts", (2, 1))],
                    ),
                    leaf("main.ts", "apps/server/main.ts", (4, 0)),
                ],
            )]
        );
    }

    #[test]
    fn preserves_leading_trailing_whitespace_in_path_segments() {
        let tree = build_diff_tree(&[file("a/file.ts", 1, 0), file(" a/file.ts", 2, 0)]);
        assert_eq!(tree.len(), 2);
        let mut names: Vec<_> = tree
            .iter()
            .filter_map(|node| match node {
                DiffTreeNode::Directory { name, path, .. } => Some((name.clone(), path.clone())),
                DiffTreeNode::File { .. } => None,
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            [(" a".into(), " a".into()), ("a".into(), "a".into())] as [(String, String); 2]
        );
    }

    #[test]
    fn keeps_its_compact_header_sticky_while_preserving_singular_labels() {
        let card = changed_files_card(
            &RunId::new("run-1").unwrap(),
            &[file("README.md", 2, 1)],
            true,
            &BTreeMap::new(),
        );
        assert_eq!(card.open_diff_label, "Open diff");
        assert_eq!(
            card.stat.unwrap().accessibility_label,
            "2 additions, 1 deletions"
        );
        assert_eq!(card.title, "1 changed file");
        assert_eq!(card.toggle_all_label, None);
        assert_eq!(card.open_diff_path.as_deref(), Some("README.md"));
    }

    #[test]
    fn shows_collapsed_folders_and_root_files_together() {
        let names = visible(
            &[
                file("apps/web/src/App.tsx", 120, 20),
                file("apps/web/src/App.test.tsx", 30, 2),
                file("packages/shared/src/git.ts", 15, 4),
                file("README.md", 3, 0),
            ],
            false,
        );
        assert_eq!(names, ["apps/web/src", "packages/shared/src", "README.md"]);
    }

    #[test]
    fn keeps_the_folder_tree_visible_when_folders_are_collapsed() {
        let card = changed_files_card(
            &RunId::new("run-1").unwrap(),
            &[file("apps/web/src/App.tsx", 120, 20)],
            false,
            &BTreeMap::new(),
        );
        assert_eq!(card.title, "1 changed file");
        assert_eq!(card.toggle_all_label.as_deref(), Some("Expand all folders"));
        assert_eq!(
            card.rows
                .iter()
                .map(|row| (row.name.as_str(), row.kind))
                .collect::<Vec<_>>(),
            [(
                "apps/web/src",
                ChangedFileRowKind::Directory { expanded: false }
            )]
        );
    }

    #[test]
    fn renders_trees_collapsed_on_the_first_render_when_collapse_all_is_active() {
        assert_eq!(
            visible(
                &[
                    file("apps/web/src/index.ts", 2, 1),
                    file("apps/web/src/main.ts", 3, 0)
                ],
                false
            ),
            ["apps/web/src"]
        );
        assert_eq!(
            visible(
                &[
                    file("apps/server/src/git/Layers/GitCore.ts", 4, 3),
                    file("apps/server/src/provider/Layers/CodexAdapter.ts", 7, 2),
                ],
                false
            ),
            ["apps/server/src"]
        );
        assert_eq!(
            visible(
                &[
                    file("README.md", 1, 0),
                    file("packages/shared/src/git.ts", 8, 2),
                    file("packages/contracts/src/orchestration.ts", 13, 3),
                ],
                false
            ),
            ["packages", "README.md"]
        );
    }

    #[test]
    fn renders_trees_expanded_on_the_first_render_when_expand_all_is_active() {
        assert_eq!(
            visible(
                &[
                    file("apps/web/src/index.ts", 2, 1),
                    file("apps/web/src/main.ts", 3, 0)
                ],
                true
            ),
            ["apps/web/src", "index.ts", "main.ts"]
        );
        assert_eq!(
            visible(
                &[
                    file("apps/server/src/git/Layers/GitCore.ts", 4, 3),
                    file("apps/server/src/provider/Layers/CodexAdapter.ts", 7, 2),
                ],
                true
            ),
            [
                "apps/server/src",
                "git/Layers",
                "GitCore.ts",
                "provider/Layers",
                "CodexAdapter.ts"
            ]
        );
        assert_eq!(
            visible(
                &[
                    file("README.md", 1, 0),
                    file("packages/shared/src/git.ts", 8, 2),
                    file("packages/contracts/src/orchestration.ts", 13, 3),
                ],
                true
            ),
            [
                "packages",
                "contracts/src",
                "orchestration.ts",
                "shared/src",
                "git.ts",
                "README.md"
            ]
        );
    }

    #[test]
    fn toggles_folders_against_the_expand_all_choice() {
        let files = [
            file("src/a.ts", 1, 0),
            file("docs/b.md", 0, 0),
            file("c.ts", 0, 0),
        ];
        let run = RunId::new("run").unwrap();
        let mut overrides = BTreeMap::new();
        toggle_directory(&mut overrides, "src", false);
        let card = changed_files_card(&run, &files, false, &overrides);
        assert_eq!(
            card.rows
                .iter()
                .map(|row| (
                    row.name.as_str(),
                    row.depth,
                    row.leading_spacer,
                    row.stat.is_some()
                ))
                .collect::<Vec<_>>(),
            [
                ("docs", 0, false, false),
                ("src", 0, false, true),
                ("a.ts", 1, true, true),
                ("c.ts", 0, true, true),
            ]
        );
        assert_eq!(card.expansion_key, "collapsed\u{0}docs\u{0}src");
        toggle_directory(&mut overrides, "src", false);
        assert_eq!(overrides.get("src"), Some(&false));
        let flat = changed_files_card(&run, &[file("a.ts", 0, 0)], false, &BTreeMap::new());
        assert!(!flat.rows[0].leading_spacer);
        assert_eq!(flat.stat, None);
    }

    #[test]
    fn formats_compact_diff_counts() {
        for (value, expected) in [
            (999, "999"),
            (1_000, "1k"),
            (1_250, "1.3k"),
            (12_345, "12k"),
            (2_000_000, "2m"),
            (15_500_000, "16m"),
            (1_500_000_000, "1.5b"),
        ] {
            assert_eq!(format_compact_diff_count(value), expected, "{value}");
        }
        assert_eq!(
            diff_stat_label(DiffStat {
                additions: 1_250,
                deletions: 3
            }),
            DiffStatLabel {
                additions: "+1.3k".into(),
                deletions: "-3".into(),
                accessibility_label: "1250 additions, 3 deletions".into(),
            }
        );
    }

    #[test]
    fn gives_each_assistant_message_its_runs_latest_checkpoint() {
        let at = Timestamp::parse("2026-10-01T10:00:00Z").unwrap();
        let message = |id: &str, run: &str, role| Message {
            notification: None,
            id: MessageId::new(id).unwrap(),
            run: Some(RunId::new(run).unwrap()),
            role,
            text: id.into(),
            attachments: vec![],
            intent: InputIntent::TurnStart,
            streaming: false,
            created_by: MessageAuthor::User,
            creation_source: "desktop".into(),
            created_at: at.clone(),
            updated_at: at.clone(),
            context: None,
        };
        let checkpoint = |id: &str, run: Option<&str>, ordinal, path: &str| Checkpoint {
            status: CheckpointStatus::Ready,
            scope: None,
            id: CheckpointId::new(id).unwrap(),
            run: run.map(|run| RunId::new(run).unwrap()),
            run_ordinal: ordinal,
            native_heads: BTreeMap::new(),
            file_ref: String::new(),
            files: vec![file(path, 1, 0)],
        };
        let state = State {
            messages: vec![
                message("user-1", "run-1", Role::User),
                message("assistant-1a", "run-1", Role::Assistant),
                message("assistant-1b", "run-1", Role::Assistant),
                message("user-2", "run-2", Role::User),
            ],
            checkpoints: vec![
                checkpoint("baseline", None, 0, "base.ts"),
                checkpoint("first", Some("run-1"), 1, "a.ts"),
                checkpoint("second", Some("run-2"), 2, "b.ts"),
                checkpoint("again", Some("run-1"), 1, "c.ts"),
            ],
            ..State::default()
        };
        let summaries = turn_diff_summaries(&state);
        assert_eq!(
            summaries
                .iter()
                .map(|s| (
                    s.checkpoint.as_str(),
                    s.assistant_message.as_ref().map(MessageId::as_str)
                ))
                .collect::<Vec<_>>(),
            [
                ("first", Some("assistant-1b")),
                ("second", None),
                ("again", Some("assistant-1b"))
            ]
        );
        let diffs = assistant_turn_diffs(&state);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].message.as_str(), "assistant-1b");
        assert_eq!(diffs[0].summary.checkpoint.as_str(), "again");
        assert_eq!(diffs[0].summary.checkpoint_turn_count, 1);
    }
}
