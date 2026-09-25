use crate::models::WorkspaceReview;
use std::collections::HashMap;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiffRow {
    pub text: String,
    pub old: Option<u64>,
    pub new: Option<u64>,
    pub kind: String,
    pub file: u64,
}

pub fn parse(source: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut old, mut new, mut file) = (None, None, 0u64);
    for line in source.lines() {
        let kind = if line.starts_with("diff --git ") {
            file += 1;
            old = None;
            new = None;
            'F'
        } else if line.starts_with("@@ ") {
            let mut parts = line.split_whitespace();
            parts.next();
            old = parts
                .next()
                .and_then(|p| p.strip_prefix('-'))
                .and_then(|p| p.split(',').next())
                .and_then(|p| p.parse().ok());
            new = parts
                .next()
                .and_then(|p| p.strip_prefix('+'))
                .and_then(|p| p.split(',').next())
                .and_then(|p| p.parse().ok());
            '@'
        } else if old.is_some() && line.starts_with('-') {
            '-'
        } else if new.is_some() && line.starts_with('+') {
            '+'
        } else if old.is_some() && line.starts_with(' ') {
            ' '
        } else {
            'M'
        };
        let old_line = if matches!(kind, '-' | ' ') { old } else { None };
        let new_line = if matches!(kind, '+' | ' ') { new } else { None };
        if old_line.is_some() {
            old = old.map(|n| n + 1);
        }
        if new_line.is_some() {
            new = new.map(|n| n + 1);
        }
        rows.push(DiffRow {
            text: line.to_owned(),
            old: old_line,
            new: new_line,
            kind: kind.to_string(),
            file,
        });
    }
    rows
}
pub fn is_file_metadata(line: &str) -> bool {
    ["index ", "--- ", "+++ "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

pub fn file_names(rows: &[DiffRow]) -> HashMap<u64, String> {
    let mut names = HashMap::new();
    for row in rows {
        let line = row.text.as_str();
        let path = if row.kind == "F" {
            line.strip_prefix("diff --git ").and_then(|header| {
                if header.starts_with('"') {
                    let (_, rest) = quoted_path(header)?;
                    let new = decode_path(rest.trim_start())?;
                    return new.strip_prefix("b/").map(str::to_owned);
                }
                // Unquoted binary patches have no +++/--- lines. With no rename
                // their two paths are identical; spaces inside a path are legal.
                let middle = header.len().checked_sub(1)? / 2;
                let old = header.get(..middle)?.strip_prefix("a/")?;
                let new = header.get(middle..)?.strip_prefix(" b/")?;
                (old == new).then(|| new.to_owned())
            })
        } else if row.kind == "M" {
            if let Some(path) = line
                .strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
            {
                decode_path(path)
            } else {
                line.strip_prefix("+++ ")
                    .or_else(|| line.strip_prefix("--- "))
                    .filter(|path| *path != "/dev/null")
                    .and_then(decode_path)
                    .map(|path| {
                        path.strip_prefix("a/")
                            .or_else(|| path.strip_prefix("b/"))
                            .unwrap_or(&path)
                            .to_owned()
                    })
            }
        } else {
            None
        };
        if let Some(path) = path {
            names.insert(row.file, path);
        }
    }
    names
}

fn decode_path(path: &str) -> Option<String> {
    if path.starts_with('"') {
        let (path, rest) = quoted_path(path)?;
        (rest.is_empty() || rest.starts_with('\t')).then_some(path)
    } else {
        Some(path.split('\t').next()?.into())
    }
}

fn quoted_path(path: &str) -> Option<(String, &str)> {
    let bytes = path.strip_prefix('"')?.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((String::from_utf8(output).ok()?, &path[i + 2..])),
            b'\\' => {
                i += 1;
                let byte = *bytes.get(i)?;
                output.push(match byte {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => 9,
                    b'n' => 10,
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => 13,
                    b'\\' | b'"' => byte,
                    b'0'..=b'7' => {
                        let mut value = u16::from(byte - b'0');
                        for _ in 0..2 {
                            if let Some(next @ b'0'..=b'7') = bytes.get(i + 1) {
                                value = value * 8 + u16::from(next - b'0');
                                i += 1;
                            } else {
                                break;
                            }
                        }
                        u8::try_from(value).ok()?
                    }
                    _ => return None,
                });
            }
            byte => output.push(byte),
        }
        i += 1;
    }
    None
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkspaceDiffFile {
    pub path: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub rows: Vec<DiffRow>,
}

pub fn diff_files(review: &WorkspaceReview) -> Vec<WorkspaceDiffFile> {
    let rows = parse(&review.diff);
    let names: HashMap<_, _> = file_names(&rows)
        .into_iter()
        .map(|(id, path)| (path, id))
        .collect();
    let mut groups = HashMap::<u64, Vec<_>>::new();
    for row in rows {
        if row.kind == "F" || (row.kind == "M" && is_file_metadata(&row.text)) {
            continue;
        }
        groups.entry(row.file).or_default().push(row);
    }
    review
        .files
        .iter()
        .map(|file| WorkspaceDiffFile {
            path: file.path.clone(),
            additions: file.additions,
            deletions: file.deletions,
            rows: names
                .get(&file.path)
                .and_then(|id| groups.remove(id))
                .unwrap_or_default(),
        })
        .collect()
}

#[cfg(kani)]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn generated_hunks_preserve_text_kind_and_line_numbers(
            files in prop::collection::vec(prop::collection::vec(
                (1u64..100_000, 1u64..100_000, prop::collection::vec(
                    (prop::sample::select(vec!['-', '+', ' ']), "[^\\r\\n]{0,24}"), 1..16,
                )), 1..4,
            ), 1..4),
        ) {
            let mut patch = String::new();
            let mut expected = Vec::new();
            for (index, hunks) in files.iter().enumerate() {
                let file = index as u64 + 1;
                for (kind, text) in [
                    ("F", format!("diff --git a/{index} b/{index}")),
                    ("M", format!("--- a/{index}")),
                    ("M", format!("+++ b/{index}")),
                ] {
                    patch.push_str(&format!("{text}\n"));
                    expected.push((text, kind.to_owned(), None, None, file));
                }
                for (old, new, changes) in hunks {
                    let old_count = changes.iter().filter(|(kind, _)| *kind != '+').count();
                    let new_count = changes.iter().filter(|(kind, _)| *kind != '-').count();
                    let header = format!("@@ -{old},{old_count} +{new},{new_count} @@");
                    patch.push_str(&format!("{header}\n"));
                    expected.push((header, "@".into(), None, None, file));
                    for (i, (kind, text)) in changes.iter().enumerate() {
                        // Count consumed source/destination lines in the generated edit script,
                        // independently of the parser's running counters.
                        let old_line = (*kind != '+').then(|| old + changes[..i].iter()
                            .filter(|(kind, _)| *kind != '+').count() as u64);
                        let new_line = (*kind != '-').then(|| new + changes[..i].iter()
                            .filter(|(kind, _)| *kind != '-').count() as u64);
                        let line = format!("{kind}{text}");
                        patch.push_str(&format!("{line}\n"));
                        expected.push((line, kind.to_string(), old_line, new_line, file));
                    }
                    let marker = "\\ No newline at end of file";
                    patch.push_str(&format!("{marker}\n"));
                    expected.push((marker.into(), "M".into(), None, None, file));
                }
            }
            let actual: Vec<_> = parse(&patch).into_iter()
                .map(|row| (row.text, row.kind, row.old, row.new, row.file)).collect();
            prop_assert_eq!(actual, expected);
        }

        #[test]
        fn octal_quoted_paths_round_trip_through_public_navigation(path in "[^\\x00/]{1,40}") {
            // Git's quoted paths encode UTF-8 bytes, not Unicode scalar values.
            let encoded: String = path.bytes().map(|byte| format!("\\{byte:03o}")).collect();
            for patch in [
                format!("diff --git \"a/{encoded}\" \"b/{encoded}\"\nBinary files differ\n"),
                format!("--- \"a/{encoded}\"\told timestamp\n+++ /dev/null\n"),
                format!("--- /dev/null\n+++ \"b/{encoded}\"\tnew timestamp\n"),
                format!("rename to \"{encoded}\"\n"),
                format!("copy to \"{encoded}\"\n"),
            ] {
                let names = file_names(&parse(&patch));
                prop_assert_eq!(names.len(), 1, "{:?}", patch);
                prop_assert_eq!(names.values().next(), Some(&path), "{:?}", patch);
            }
        }
    }

    #[rstest::rstest]
    #[case::unterminated("\"unterminated")]
    #[case::dangling_escape("\"trailing\\")]
    #[case::unknown_escape("\"bad\\q\"")]
    #[case::octal_overflow("\"\\400\"")]
    #[case::invalid_utf8("\"\\377\"")]
    #[case::trailing_junk("\"valid\"junk")]
    fn malformed_quoted_paths_do_not_create_navigation_targets(#[case] path: &str) {
        let rows = parse(&format!("+++ {path}\n"));
        assert!(file_names(&rows).is_empty());
    }

    #[test]
    fn file_navigation_matches_real_git_paths_including_renames_binary_and_unicode() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "--quiet"]);
        let names = [
            "a.txt",
            "space name.txt",
            "日本語.txt",
            "tab\tname.txt",
            "quoted\"name.txt",
            "line\nname.txt",
            "controls\u{7}\u{8}\u{b}\u{c}\r\\.txt",
        ];
        for name in names {
            std::fs::write(root.join(name), "before\n").unwrap();
        }
        std::fs::write(root.join("binary.dat"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("old.txt"), "rename me\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
        for name in names {
            std::fs::write(root.join(name), "after\n").unwrap();
        }
        std::fs::write(root.join("binary.dat"), [0, 3, 4]).unwrap();
        git(&["mv", "old.txt", "renamed 日本語.txt"]);
        for quoting in ["core.quotePath=true", "core.quotePath=false"] {
            let patch = git(&[
                "-c",
                quoting,
                "diff",
                "--no-ext-diff",
                "--no-color",
                "HEAD",
                "--",
            ]);
            let rows = parse(&patch);
            for name in names
                .into_iter()
                .chain(["binary.dat", "renamed 日本語.txt"])
            {
                assert!(
                    file_names(&rows).values().any(|path| path.as_str() == name),
                    "cannot navigate to {name:?} ({quoting})"
                );
            }
            assert!(
                !file_names(&rows)
                    .values()
                    .any(|path| path.as_str() == "not-present.txt")
            );
        }
    }

    #[test]
    fn review_groups_rows_and_keeps_binary_and_deleted_files() {
        let review: crate::models::WorkspaceReview = serde_json::from_value(serde_json::json!({
            "branch":"main", "additions":1, "deletions":1,
            "files":[{"path":"removed.txt","status":"D","additions":0,"deletions":1},
                     {"path":"new.txt","status":"?","additions":1,"deletions":0},
                     {"path":"binary.dat","status":"M","additions":null,"deletions":null}],
            "diff":"diff --git a/removed.txt b/removed.txt\n--- a/removed.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\ndiff --git a/new.txt b/new.txt\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+new\ndiff --git a/binary.dat b/binary.dat\nBinary files a/binary.dat and b/binary.dat differ\n"
        })).unwrap();
        let files = diff_files(&review);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].rows[1].text, "-gone");
        assert_eq!(files[0].rows[1].old, Some(1));
        assert_eq!(files[1].rows[1].new, Some(1));
        assert_eq!(files[1].additions, Some(1));
        assert!(files[2].rows[0].text.starts_with("Binary files "));
        assert_eq!(files[2].additions, None);
        assert!(
            files
                .iter()
                .flat_map(|file| &file.rows)
                .all(|row| row.kind != "F" && !row.text.starts_with("+++ "))
        );
    }

    #[test]
    fn line_numbers_reset_for_each_file_and_keep_deleted_lines() {
        let rows = parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -3,2 +3,2 @@\n-old\n+new\n context\ndiff --git a/b b/b\n@@ -0,0 +1 @@\n+added\n",
        );
        assert_eq!((rows[4].old, rows[4].new), (Some(3), None));
        assert_eq!((rows[5].old, rows[5].new), (None, Some(3)));
        assert_eq!((rows[6].old, rows[6].new), (Some(4), Some(4)));
        assert_eq!((rows[9].file, rows[9].new), (2, Some(1)));
    }
}
