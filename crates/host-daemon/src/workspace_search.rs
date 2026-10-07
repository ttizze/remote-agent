//! Path search for `@` mentions: an index of a directory's files and folders
//! that honors ignore files, ranked by how closely each path matches.
use agent_protocol::workspace::{EntryKind, EntrySearch, SearchEntries, WorkspaceEntry};
use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 25_000;
const MAX_LIMIT: u32 = 200;
const MAX_QUERY: usize = 256;
/// A rebuilt index sees files the agent or the user just wrote.
const INDEX_LIFETIME: Duration = Duration::from_secs(15);
const MAX_INDEXES: usize = 16;
/// Never listed, even outside a Git checkout.
const EXCLUDED: [&str; 3] = [".git", "node_modules", ".convex"];
const IMAGE_EXTENSIONS: [&str; 8] = [
    ".avif", ".gif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".webp",
];

type Index = Arc<Vec<WorkspaceEntry>>;

#[derive(Default)]
pub(crate) struct WorkspaceSearch {
    indexes: Mutex<HashMap<PathBuf, (Instant, Index)>>,
}

/// Every file and directory under `root`, sorted, at most 25,000.
fn build(root: &Path) -> Vec<WorkspaceEntry> {
    let mut entries = vec![];
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| {
            !EXCLUDED
                .iter()
                .any(|excluded| entry.file_name() == *excluded)
        })
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    for entry in walker.flatten() {
        if entries.len() == MAX_ENTRIES {
            break;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let path = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if path.is_empty() {
            continue;
        }
        let kind = if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        entries.push(WorkspaceEntry { path, kind });
    }
    entries
}

/// The edit distance with adjacent transpositions.
fn typo_distance(a: &[char], b: &[char]) -> usize {
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        rows[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// The gaps of `query` as a subsequence of `text`, or `None`.
fn subsequence_gaps(query: &str, text: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    let mut first = None;
    let mut last = 0;
    let mut gaps = 0;
    for wanted in query.chars() {
        let (index, _) = chars.find(|(_, c)| *c == wanted)?;
        if first.is_some() {
            gaps += index - last - 1;
        }
        first.get_or_insert(index);
        last = index;
    }
    Some(gaps + first.unwrap_or(0))
}

/// Lower is closer; `None` does not match.
fn score(query: &str, path: &str) -> Option<usize> {
    let path = path.to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    if name == query {
        return Some(0);
    }
    if name.starts_with(query) {
        return Some(100 + (name.len() - query.len()).min(64));
    }
    if let Some(index) = name.find(query) {
        return Some(200 + index);
    }
    if let Some(index) = path.find(query) {
        return Some(300 + index.min(99));
    }
    if let Some(gaps) = subsequence_gaps(query, &path) {
        return Some(400 + gaps.min(199));
    }
    let wanted: Vec<char> = query.chars().collect();
    if wanted.len() < 4 {
        return None;
    }
    let allowed = if wanted.len() >= 8 { 2 } else { 1 };
    let stem: Vec<char> = name.chars().collect();
    let candidates = [
        stem.clone(),
        stem.iter().take(wanted.len()).copied().collect::<Vec<_>>(),
    ];
    candidates
        .iter()
        .map(|candidate| typo_distance(&wanted, candidate))
        .min()
        .filter(|distance| *distance <= allowed)
        .map(|distance| 600 + distance)
}

fn image(path: &str) -> bool {
    let lower = path.to_lowercase();
    IMAGE_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// Ranked entries of the index; the kind and image filters apply before the limit.
fn search(index: &[WorkspaceEntry], request: &SearchEntries) -> EntrySearch {
    let query = request
        .query
        .trim()
        .trim_start_matches(['@', '.', '/'])
        .to_lowercase();
    let limit = request.limit as usize;
    let wanted = |entry: &&WorkspaceEntry| {
        if request.image_only {
            return entry.kind == EntryKind::File && image(&entry.path);
        }
        request.kind.is_none_or(|kind| entry.kind == kind)
    };
    let mut matched: Vec<(usize, &WorkspaceEntry)> = index
        .iter()
        .filter(wanted)
        .filter_map(|entry| {
            if query.is_empty() {
                Some((0, entry))
            } else {
                score(&query, &entry.path).map(|score| (score, entry))
            }
        })
        .collect();
    if !query.is_empty() {
        matched.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.path.len().cmp(&b.1.path.len()))
                .then_with(|| a.1.path.cmp(&b.1.path))
        });
    }
    EntrySearch {
        truncated: matched.len() > limit,
        entries: matched
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry.clone())
            .collect(),
    }
}

impl WorkspaceSearch {
    pub(crate) async fn search(&self, request: SearchEntries) -> Result<EntrySearch> {
        if request.limit == 0 || request.limit > MAX_LIMIT {
            return Err(anyhow!("search limit must be 1 to {MAX_LIMIT}"));
        }
        if request.query.trim().chars().count() > MAX_QUERY {
            return Err(anyhow!("search query exceeds {MAX_QUERY} characters"));
        }
        let root = tokio::fs::canonicalize(request.cwd.trim()).await?;
        let root = dunce::simplified(&root).to_owned();
        if !root.is_dir() {
            return Err(anyhow!("workspace root is not a directory"));
        }
        let cached = self
            .indexes
            .lock()
            .unwrap()
            .get(&root)
            .filter(|(built, _)| built.elapsed() < INDEX_LIFETIME)
            .map(|(_, index)| index.clone());
        let index = match cached {
            Some(index) => index,
            None => {
                let walked = root.clone();
                let index: Index =
                    Arc::new(tokio::task::spawn_blocking(move || build(&walked)).await?);
                let mut indexes = self.indexes.lock().unwrap();
                indexes.retain(|_, (built, _)| built.elapsed() < INDEX_LIFETIME);
                if indexes.len() >= MAX_INDEXES {
                    indexes.clear();
                }
                indexes.insert(root, (Instant::now(), index.clone()));
                index
            }
        };
        Ok(search(&index, &request))
    }
}

#[cfg(test)]
mod tests;
