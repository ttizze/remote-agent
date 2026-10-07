//! A Git diff too large to send whole, shown file by file: its file list with
//! the counts the Host sent, and each file's patch read on its own when the
//! reader comes near it.
use crate::{
    presentation::diff::{DiffRow, file_names, is_file_metadata, parse},
    state::{DiffFilePatch, DiffFilesEntry, Snapshot},
    view::{
        checkpoints::{DiffPanelSelection, DiffSelection},
        collation::numeric_locale_compare,
        timeline::rows::TimelineLayout,
    },
};
use agent_protocol::workspace as w;

/// Patches read at the same time.
pub(crate) const CONCURRENT_FILE_READS: usize = 4;
/// What a single-file reply without the requested source reads as.
pub(crate) const MISSING_SOURCE: &str = "Diff no longer available. Refresh the comparison.";

/// How far a file's patch can be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ReviewFileStatus {
    /// Its patch has not arrived yet.
    Loading,
    Loaded,
    /// Its patch exceeded the size limit: the rows are a part, the counts whole.
    Truncated,
    /// Its patch could not be read.
    Failed,
    /// The Host answered without this file's patch.
    Unavailable,
}

/// One file of the list, with its patch once read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReviewFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
    pub status: ReviewFileStatus,
    /// Desktop: the header's status button label. Mobile: the notice the file
    /// shows above its rows.
    pub notice: Option<String>,
    /// The patch's lines without the file's header lines.
    pub rows: Vec<DiffRow>,
}

/// The files of a diff too large to send whole.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReviewFilesView {
    /// Every file, desktop by path and mobile in the Host's order.
    pub files: Vec<ReviewFile>,
    /// The leading files whose patch arrived or failed. Desktop shows the
    /// settled files and a loading boundary for the rest.
    pub settled_count: u64,
    /// Placeholder headers the loading boundary shows.
    pub placeholder_count: u64,
    /// Totals of the file list, which counts every change.
    pub additions: u64,
    pub deletions: u64,
    /// A patch is being read.
    pub pending: bool,
}

/// What a reader's movement asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileRequest<'a> {
    /// The source was just shown.
    Initial,
    /// The loading boundary came near.
    Next,
    /// A file was revealed or scrolled to.
    Reveal(&'a str),
}

fn batch(layout: TimelineLayout) -> usize {
    match layout {
        TimelineLayout::Desktop => 4,
        TimelineLayout::Mobile => 3,
    }
}

/// Indices of `files` in the panel's order: desktop by path, digits by value
/// and case ignored; mobile as the Host listed them.
pub(crate) fn file_order(files: &[w::DiffFile], layout: TimelineLayout) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    if layout == TimelineLayout::Desktop {
        order.sort_by(|a, b| numeric_locale_compare(&files[*a].path, &files[*b].path));
    }
    order
}

fn settled(patch: Option<&DiffFilePatch>) -> bool {
    patch.is_some_and(|patch| patch.result.is_some())
}

/// The leading files, in the panel's order, whose patch arrived or failed.
pub(crate) fn settled_count(entry: &DiffFilesEntry, order: &[usize]) -> usize {
    order
        .iter()
        .position(|index| !settled(entry.patches.get(&entry.files[*index].path)))
        .unwrap_or(order.len())
}

/// The files `request` asks for that were not asked for yet, in order.
///
/// Desktop asks for the first four, then four after the settled files each
/// time the boundary comes near, and a revealed file past them alone. Mobile
/// asks for the first three, and a visible file with the two after it.
pub(crate) fn wanted_files(entry: &DiffFilesEntry, request: FileRequest) -> Vec<usize> {
    let order = file_order(&entry.files, entry.layout);
    let size = batch(entry.layout);
    let positions = match request {
        FileRequest::Initial => 0..size,
        FileRequest::Next => {
            let start = settled_count(entry, &order);
            start..start + size
        }
        FileRequest::Reveal(path) => {
            let Some(position) = order
                .iter()
                .position(|index| entry.files[*index].path == path)
            else {
                return vec![];
            };
            match entry.layout {
                TimelineLayout::Desktop if position < settled_count(entry, &order) => 0..0,
                TimelineLayout::Desktop => position..position + 1,
                TimelineLayout::Mobile => position..position + size,
            }
        }
    };
    positions
        .filter_map(|position| order.get(position).copied())
        .filter(|index| !entry.patches.contains_key(&entry.files[*index].path))
        .collect()
}

/// Whether retrying `path` reads it again: desktop retries a file it could
/// not show, mobile one whose read failed.
pub(crate) fn retries(entry: &DiffFilesEntry, path: &str) -> bool {
    let Some(patch) = entry.patches.get(path).filter(|patch| !patch.in_flight) else {
        return false;
    };
    let Some(file) = entry.files.iter().find(|file| file.path == path) else {
        return false;
    };
    match (resolve(Some(patch), file).0, entry.layout) {
        (ReviewFileStatus::Failed, _) => true,
        (ReviewFileStatus::Unavailable, layout) => layout == TimelineLayout::Desktop,
        _ => false,
    }
}

/// The rows of `file` in a single-file reply: the patch's file of that path,
/// or its only file when the reply lists only that path.
fn file_rows(source: &w::DiffSource, file: &w::DiffFile) -> Option<Vec<DiffRow>> {
    let rows = parse(&source.diff);
    let names = file_names(&rows);
    let id = names
        .iter()
        .find(|(_, name)| **name == file.path)
        .map(|(id, _)| *id)
        .or_else(|| {
            let mut ids = rows
                .iter()
                .filter(|row| row.kind == "F")
                .map(|row| row.file);
            let only = ids.next()?;
            let listed = source.files.as_deref()?;
            (ids.next().is_none() && listed.len() == 1 && listed[0].path == file.path)
                .then_some(only)
        })?;
    Some(
        rows.into_iter()
            .filter(|row| {
                row.file == id
                    && row.kind != "F"
                    && !(row.kind == "M" && is_file_metadata(&row.text))
            })
            .collect(),
    )
}

/// The file's status and the rows it can show.
fn resolve(patch: Option<&DiffFilePatch>, file: &w::DiffFile) -> (ReviewFileStatus, Vec<DiffRow>) {
    match patch.and_then(|patch| patch.result.as_ref()) {
        None => (ReviewFileStatus::Loading, vec![]),
        Some(Err(_)) => (ReviewFileStatus::Failed, vec![]),
        Some(Ok(source)) => match file_rows(source, file) {
            None => (ReviewFileStatus::Unavailable, vec![]),
            Some(rows) if source.truncated => (ReviewFileStatus::Truncated, rows),
            Some(rows) => (ReviewFileStatus::Loaded, rows),
        },
    }
}

fn notice(status: ReviewFileStatus, layout: TimelineLayout) -> Option<&'static str> {
    use ReviewFileStatus as S;
    match (layout, status) {
        (_, S::Loaded) | (TimelineLayout::Desktop, S::Loading) => None,
        (TimelineLayout::Desktop, S::Failed | S::Unavailable) => Some("Retry loading diff"),
        (TimelineLayout::Desktop, S::Truncated) => {
            Some("This file is too large to show in full. Counts include all changes.")
        }
        (TimelineLayout::Mobile, S::Loading) => Some("Loading diff\u{2026}"),
        (TimelineLayout::Mobile, S::Failed) => {
            Some("Could not load diff. Select the file to retry.")
        }
        (TimelineLayout::Mobile, S::Unavailable) => Some("Could not display file preview."),
        (TimelineLayout::Mobile, S::Truncated) => {
            Some("File preview exceeds the size limit. Counts include all changes.")
        }
    }
}

/// The per-file patches of the source `selection` shows in `cwd`, while that
/// source is truncated and lists its files.
pub(crate) fn lazy_entry<'a>(
    snapshot: &'a Snapshot,
    cwd: &str,
    selection: &DiffPanelSelection,
) -> Option<&'a DiffFilesEntry> {
    let kind = match selection.selection {
        DiffSelection::Unstaged => w::DiffSourceKind::WorkingTree,
        DiffSelection::Branch { .. } => w::DiffSourceKind::BranchRange,
        DiffSelection::Turn { .. } => return None,
    };
    let preview = snapshot
        .sources
        .diff_preview
        .as_ref()
        .filter(|entry| entry.request.cwd == cwd)?;
    let source = preview
        .result
        .as_ref()?
        .sources
        .iter()
        .find(|source| source.kind == kind)?;
    snapshot.sources.diff_files.as_ref().filter(|entry| {
        source.truncated
            && source.files.is_some()
            && entry.preview == preview.request
            && entry.kind == kind
            && entry.diff_hash == source.diff_hash
    })
}

/// The file list of a source too large to send whole, with each file's patch
/// as far as it has arrived.
pub(crate) fn review_files_view(entry: &DiffFilesEntry) -> ReviewFilesView {
    let order = file_order(&entry.files, entry.layout);
    let settled = settled_count(entry, &order);
    let files: Vec<ReviewFile> = order
        .iter()
        .map(|index| {
            let file = &entry.files[*index];
            let (status, rows) = resolve(entry.patches.get(&file.path), file);
            ReviewFile {
                path: file.path.clone(),
                previous_path: file.previous_path.clone(),
                additions: file.additions,
                deletions: file.deletions,
                status,
                notice: notice(status, entry.layout).map(str::to_owned),
                rows,
            }
        })
        .collect();
    ReviewFilesView {
        settled_count: settled as u64,
        placeholder_count: (files.len() - settled).min(batch(TimelineLayout::Desktop)) as u64,
        additions: entry.files.iter().map(|file| file.additions).sum(),
        deletions: entry.files.iter().map(|file| file.deletions).sum(),
        pending: entry.pending(),
        files,
    }
}

#[cfg(test)]
mod tests;
