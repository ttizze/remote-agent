use super::*;
use std::sync::Arc;

fn stat(path: &str, additions: u64, deletions: u64) -> w::DiffFile {
    w::DiffFile {
        path: path.into(),
        previous_path: None,
        additions,
        deletions,
    }
}

fn entry(layout: TimelineLayout, paths: &[&str]) -> DiffFilesEntry {
    DiffFilesEntry {
        preview: w::DiffPreview {
            cwd: "/repo".into(),
            base_ref: None,
            ignore_whitespace: false,
            file: None,
        },
        kind: w::DiffSourceKind::BranchRange,
        base_ref: Some("main".into()),
        diff_hash: "hash".into(),
        generated_at: agent_domain::Timestamp::from_millis(0).unwrap(),
        layout,
        files: paths.iter().map(|path| stat(path, 1, 0)).collect(),
        patches: Default::default(),
        queue: vec![],
        superseded: Default::default(),
        revision: 0,
    }
}

fn patch(path: &str) -> String {
    format!(
        "diff --git a/{path} b/{path}\nindex 1..2 100644\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+new\n"
    )
}

fn source(diff: String, files: Vec<w::DiffFile>, truncated: bool) -> w::DiffSource {
    w::DiffSource {
        id: "branch-range".into(),
        kind: w::DiffSourceKind::BranchRange,
        title: "Changes vs main".into(),
        base_ref: Some("main".into()),
        head_ref: Some("topic".into()),
        diff,
        diff_hash: "file".into(),
        truncated,
        files: Some(files),
    }
}

fn settle(entry: &mut DiffFilesEntry, path: &str, result: Result<w::DiffSource, &str>) {
    entry.patches.insert(
        path.into(),
        DiffFilePatch {
            result: Some(result.map(Arc::new).map_err(str::to_owned)),
            in_flight: false,
        },
    );
}

fn request(entry: &mut DiffFilesEntry, path: &str) {
    entry.patches.insert(path.into(), DiffFilePatch::default());
}

fn loaded(entry: &mut DiffFilesEntry, path: &str) {
    settle(
        entry,
        path,
        Ok(source(patch(path), vec![stat(path, 1, 1)], false)),
    );
}

fn paths(entry: &DiffFilesEntry, indices: Vec<usize>) -> Vec<&str> {
    indices
        .into_iter()
        .map(|index| entry.files[index].path.as_str())
        .collect()
}

// web useReviewFilePatches: files sorted by path with numeric, case-blind
// collation; indices 0-3 first, then four after the settled files.
#[test]
fn desktop_reads_the_first_four_files_by_path_then_four_after_the_settled_ones() {
    let mut files = entry(
        TimelineLayout::Desktop,
        &[
            "src/b10.ts",
            "src/B2.ts",
            "a.ts",
            "c.ts",
            "d.ts",
            "e.ts",
            "f.ts",
            "g.ts",
            "h.ts",
        ],
    );
    let first = wanted_files(&files, FileRequest::Initial);
    assert_eq!(paths(&files, first), ["a.ts", "c.ts", "d.ts", "e.ts"]);
    for path in ["a.ts", "c.ts", "d.ts", "e.ts"] {
        request(&mut files, path);
    }
    assert!(wanted_files(&files, FileRequest::Initial).is_empty());
    // Nothing settled yet: the next batch is the one already asked for.
    assert!(wanted_files(&files, FileRequest::Next).is_empty());
    loaded(&mut files, "a.ts");
    loaded(&mut files, "c.ts");
    settle(&mut files, "e.ts", Err("offline"));
    let next = wanted_files(&files, FileRequest::Next);
    assert_eq!(paths(&files, next), ["f.ts", "g.ts"]);
    loaded(&mut files, "d.ts");
    let next = wanted_files(&files, FileRequest::Next);
    assert_eq!(paths(&files, next), ["f.ts", "g.ts", "h.ts", "src/B2.ts"]);
}

// web DiffPanel revealDiffFile: a file past the settled ones is requested
// alone; one inside them is not asked again.
#[test]
fn desktop_reads_a_revealed_file_past_the_settled_files_alone() {
    let mut files = entry(
        TimelineLayout::Desktop,
        &["a.ts", "b.ts", "c.ts", "d.ts", "e.ts", "f.ts", "g.ts"],
    );
    for path in ["a.ts", "b.ts", "c.ts", "d.ts"] {
        loaded(&mut files, path);
    }
    let revealed = wanted_files(&files, FileRequest::Reveal("f.ts"));
    assert_eq!(paths(&files, revealed), ["f.ts"]);
    assert!(wanted_files(&files, FileRequest::Reveal("b.ts")).is_empty());
    assert!(wanted_files(&files, FileRequest::Reveal("missing.ts")).is_empty());
}

// mobile useReviewDiffData: the Host's order, indices 0-2 first, and a
// visible file with the two after it.
#[test]
fn mobile_reads_the_first_three_files_in_host_order_and_a_visible_file_with_the_two_after_it() {
    let mut files = entry(
        TimelineLayout::Mobile,
        &["z.ts", "a.ts", "m.ts", "b.ts", "c.ts", "d.ts"],
    );
    let first = wanted_files(&files, FileRequest::Initial);
    assert_eq!(paths(&files, first), ["z.ts", "a.ts", "m.ts"]);
    for path in ["z.ts", "a.ts", "m.ts"] {
        request(&mut files, path);
    }
    let visible = wanted_files(&files, FileRequest::Reveal("m.ts"));
    assert_eq!(paths(&files, visible), ["b.ts", "c.ts"]);
    let visible = wanted_files(&files, FileRequest::Reveal("d.ts"));
    assert_eq!(paths(&files, visible), ["d.ts"]);
}

// web DiffFileStatus retries a file it could not show; mobile
// loadVisibleFile(fileId, true) retries only a failed read.
#[test]
fn retry_reads_a_failed_file_again_and_on_desktop_an_unavailable_one() {
    for layout in [TimelineLayout::Desktop, TimelineLayout::Mobile] {
        let mut files = entry(layout, &["a.ts", "b.ts", "c.ts", "d.ts"]);
        settle(&mut files, "a.ts", Err("offline"));
        settle(
            &mut files,
            "b.ts",
            Ok(source(
                patch("other.ts"),
                vec![stat("other.ts", 1, 1)],
                false,
            )),
        );
        loaded(&mut files, "c.ts");
        request(&mut files, "d.ts");
        assert!(retries(&files, "a.ts"));
        assert_eq!(retries(&files, "b.ts"), layout == TimelineLayout::Desktop);
        assert!(!retries(&files, "c.ts"));
        assert!(!retries(&files, "d.ts"));
    }
}

// web useReviewFilePatches fileStates and mobile getCachedReviewFile: rows of
// the requested path, a single-file reply under its listed path, a partial
// patch, a failure and a reply without the file.
#[test]
fn each_file_shows_its_patch_or_why_it_cannot() {
    let mut files = entry(
        TimelineLayout::Desktop,
        &["a.ts", "b.ts", "c.ts", "d.ts", "e.ts", "f.ts"],
    );
    files.files[0].additions = 40;
    loaded(&mut files, "a.ts");
    // A reply whose patch names the file differently but lists only it.
    settle(
        &mut files,
        "b.ts",
        Ok(source(patch("renamed.ts"), vec![stat("b.ts", 1, 1)], false)),
    );
    settle(
        &mut files,
        "c.ts",
        Ok(source(patch("c.ts"), vec![stat("c.ts", 1, 1)], true)),
    );
    settle(&mut files, "d.ts", Err("offline"));
    settle(&mut files, "e.ts", Ok(source(String::new(), vec![], false)));
    let view = review_files_view(&files);
    let statuses: Vec<_> = view.files.iter().map(|file| file.status).collect();
    assert_eq!(
        statuses,
        [
            ReviewFileStatus::Loaded,
            ReviewFileStatus::Loaded,
            ReviewFileStatus::Truncated,
            ReviewFileStatus::Failed,
            ReviewFileStatus::Unavailable,
            ReviewFileStatus::Loading,
        ]
    );
    let lines: Vec<_> = view.files[0]
        .rows
        .iter()
        .map(|row| (row.kind.as_str(), row.text.as_str()))
        .collect();
    assert_eq!(lines, [("@", "@@ -1 +1 @@"), ("-", "-old"), ("+", "+new")]);
    assert_eq!(view.files[1].rows.len(), 3);
    assert_eq!(view.files[2].rows.len(), 3);
    assert!(view.files[3].rows.is_empty() && view.files[4].rows.is_empty());
    assert_eq!((view.files[0].additions, view.files[0].deletions), (40, 0));
}

// web DiffPanel: totals from the file list, the boundary after the settled
// files with up to four placeholder headers, and the status button labels.
#[test]
fn desktop_lists_settled_files_before_a_boundary_with_totals_of_the_whole_list() {
    let mut files = entry(
        TimelineLayout::Desktop,
        &["a.ts", "b.ts", "c.ts", "d.ts", "e.ts", "f.ts", "g.ts"],
    );
    files.files[6].deletions = 9;
    loaded(&mut files, "a.ts");
    settle(&mut files, "b.ts", Err("offline"));
    settle(
        &mut files,
        "c.ts",
        Ok(source(patch("c.ts"), vec![stat("c.ts", 1, 1)], true)),
    );
    request(&mut files, "d.ts");
    files.patches.get_mut("d.ts").unwrap().in_flight = true;
    let view = review_files_view(&files);
    assert_eq!(view.settled_count, 3);
    assert_eq!(view.placeholder_count, 4);
    assert_eq!((view.additions, view.deletions), (7, 9));
    assert!(view.pending);
    let notices: Vec<_> = view.files[..4]
        .iter()
        .map(|file| file.notice.as_deref())
        .collect();
    assert_eq!(
        notices,
        [
            None,
            Some("Retry loading diff"),
            Some("This file is too large to show in full. Counts include all changes."),
            None,
        ]
    );
    for path in ["d.ts", "e.ts", "f.ts"] {
        loaded(&mut files, path);
    }
    let view = review_files_view(&files);
    assert_eq!((view.settled_count, view.placeholder_count), (6, 1));
    assert!(!view.pending);
}

// mobile getCachedReviewFile: every file listed in the Host's order with the
// notice of its state.
#[test]
fn mobile_lists_every_file_with_the_notice_of_its_state() {
    let mut files = entry(
        TimelineLayout::Mobile,
        &["e.ts", "d.ts", "c.ts", "b.ts", "a.ts"],
    );
    loaded(&mut files, "e.ts");
    settle(&mut files, "d.ts", Err("offline"));
    settle(&mut files, "c.ts", Ok(source(String::new(), vec![], false)));
    settle(
        &mut files,
        "b.ts",
        Ok(source(patch("b.ts"), vec![stat("b.ts", 1, 1)], true)),
    );
    let view = review_files_view(&files);
    let listed: Vec<_> = view
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.notice.as_deref()))
        .collect();
    assert_eq!(
        listed,
        [
            ("e.ts", None),
            (
                "d.ts",
                Some("Could not load diff. Select the file to retry.")
            ),
            ("c.ts", Some("Could not display file preview.")),
            (
                "b.ts",
                Some("File preview exceeds the size limit. Counts include all changes.")
            ),
            ("a.ts", Some("Loading diff\u{2026}")),
        ]
    );
}

#[test]
fn file_order_keeps_every_file_once() {
    use proptest::prelude::*;
    proptest!(|(names in proptest::collection::vec("[a-cA-C0-9/._]{1,6}", 0..12))| {
        let listed: Vec<_> = names.iter().map(|name| stat(name, 0, 0)).collect();
        for layout in [TimelineLayout::Desktop, TimelineLayout::Mobile] {
            let mut order = file_order(&listed, layout);
            order.sort_unstable();
            prop_assert_eq!(order, (0..listed.len()).collect::<Vec<_>>());
        }
    });
}
