use super::*;

fn write(root: &Path, path: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "").unwrap();
}

fn git(root: &Path, args: &[&str]) {
    crate::git::text(root, args).unwrap();
}

async fn run(root: &Path, query: &str, limit: u32, kind: Option<EntryKind>) -> EntrySearch {
    WorkspaceSearch::default()
        .search(SearchEntries {
            cwd: root.to_string_lossy().into_owned(),
            query: query.into(),
            limit,
            kind,
            image_only: false,
        })
        .await
        .unwrap()
}

fn paths(result: &EntrySearch) -> Vec<&str> {
    result
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect()
}

// WorkspaceEntries.test.ts "returns files and directories relative to cwd".
#[tokio::test]
async fn lists_files_and_directories_relative_to_the_directory() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for path in [
        "src/components/Composer.tsx",
        "src/index.ts",
        "README.md",
        ".git/HEAD",
        "node_modules/pkg/index.js",
    ] {
        write(root, path);
    }
    let result = run(root, "", 100, None).await;
    let found = paths(&result);
    for expected in [
        "src",
        "src/components",
        "src/components/Composer.tsx",
        "README.md",
    ] {
        assert!(found.contains(&expected), "{expected}: {found:?}");
    }
    assert!(found.iter().all(|path| !path.starts_with(".git")));
    assert!(found.iter().all(|path| !path.starts_with("node_modules")));
    assert!(!result.truncated);
}

// "filters and ranks entries by query", "supports fuzzy subsequence queries for
// composer path search", "prioritizes exact basename matches ahead of broader
// path matches" and "tracks truncation without sorting every fuzzy match".
#[tokio::test]
async fn ranks_substring_fuzzy_and_exact_name_matches() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for path in [
        "src/components/Composer.tsx",
        "src/components/composePrompt.ts",
        "docs/composition.md",
        "docs/composer.tsx-notes.md",
    ] {
        write(root, path);
    }
    let result = run(root, "compo", 5, None).await;
    assert!(paths(&result).contains(&"src/components"));
    assert!(
        paths(&result)
            .iter()
            .all(|path| path.to_lowercase().contains("compo"))
    );
    let fuzzy = run(root, "cmp", 10, None).await;
    assert!(paths(&fuzzy).contains(&"src/components"));
    assert!(paths(&fuzzy).contains(&"src/components/Composer.tsx"));
    let exact = run(root, "Composer.tsx", 10, None).await;
    assert_eq!(exact.entries[0].path, "src/components/Composer.tsx");
    let one = run(root, "cmp", 1, None).await;
    assert_eq!((one.entries.len(), one.truncated), (1, true));
    // "supports typo-resistant file search through fff"
    assert!(paths(&run(root, "compoesr", 10, None).await).contains(&"src/components/Composer.tsx"));
}

// "applies the file filter before limiting search results", "answers an empty
// file-filtered query with a bounded file listing" and "returns only directories
// for the directory filter".
#[tokio::test]
async fn kind_filters_apply_before_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for path in ["src/index.ts", "src/internal.ts"] {
        write(root, path);
    }
    let files = run(root, "src", 1, Some(EntryKind::File)).await;
    assert_eq!(files.entries.len(), 1);
    assert_eq!(files.entries[0].kind, EntryKind::File);
    assert!(["src/index.ts", "src/internal.ts"].contains(&files.entries[0].path.as_str()));
    assert!(files.truncated);
    let directories = run(root, "src", 10, Some(EntryKind::Directory)).await;
    assert_eq!(
        directories.entries,
        [WorkspaceEntry {
            path: "src".into(),
            kind: EntryKind::Directory
        }]
    );
    assert!(!directories.truncated);

    let listing = tempfile::tempdir().unwrap();
    write(listing.path(), "src/index.ts");
    write(listing.path(), "README.md");
    let mut found = paths(&run(listing.path(), "", 10, Some(EntryKind::File)).await)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    found.sort();
    assert_eq!(found, ["README.md", "src/index.ts"]);
}

// "excludes gitignored paths for git repositories", "excludes tracked paths that
// match ignore rules" and "excludes .convex in non-git workspaces".
#[tokio::test]
async fn ignored_paths_are_not_offered() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet"]);
    std::fs::write(root.join(".gitignore"), ".convex/\nconvex/\nignored.txt\n").unwrap();
    for path in [
        "src/keep.ts",
        "ignored.txt",
        ".convex/local-storage/data.json",
        "convex/modules/data.json",
    ] {
        write(root, path);
    }
    let found = run(root, "", 100, None).await;
    let found = paths(&found);
    assert!(found.contains(&"src/keep.ts"));
    assert!(!found.contains(&"ignored.txt"));
    assert!(found.iter().all(|path| !path.starts_with(".convex")));
    assert!(found.iter().all(|path| !path.starts_with("convex/")));

    let plain = tempfile::tempdir().unwrap();
    write(plain.path(), ".convex/local-storage/data.json");
    write(plain.path(), "src/keep.ts");
    let found = run(plain.path(), "", 100, None).await;
    assert!(paths(&found).contains(&"src/keep.ts"));
    assert!(
        paths(&found)
            .iter()
            .all(|path| !path.starts_with(".convex"))
    );
}

// WorkspaceSearchIndex.test.ts "filters image searches before applying the
// result limit".
#[tokio::test]
async fn image_searches_filter_before_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for index in 0..200 {
        write(root, &format!("src/file-{index}.ts"));
    }
    write(root, "public/icon.svg");
    let result = WorkspaceSearch::default()
        .search(SearchEntries {
            cwd: root.to_string_lossy().into_owned(),
            query: String::new(),
            limit: 1,
            kind: Some(EntryKind::Directory),
            image_only: true,
        })
        .await
        .unwrap();
    assert_eq!(
        result.entries,
        [WorkspaceEntry {
            path: "public/icon.svg".into(),
            kind: EntryKind::File
        }]
    );
}

#[tokio::test]
async fn finds_a_path_after_the_first_twenty_five_thousand_entries() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join("a")).unwrap();
    for index in 0..25_000 {
        std::fs::write(root.join("a").join(index.to_string()), "").unwrap();
    }
    write(root, "z/needle.rs");
    let result = run(root, "needle.rs", 5, None).await;
    assert_eq!(paths(&result), ["z/needle.rs"]);
    assert!(!result.truncated);
}
