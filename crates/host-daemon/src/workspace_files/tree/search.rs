//! The same FFF release and scoring options used by T3's
//! WorkspaceSearchIndex. The Host owns the index/watchers; no Node runtime is involved.
use super::*;
use agent_protocol::file_tree::SearchTreeEntries;
use fff_search::{
    FFFMode, FilePicker, FilePickerOptions, FileSearchConfig, FuzzySearchOptions, MixedItemRef,
    MixedSearchConfig, PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};
use std::sync::OnceLock;

const IDLE_TTL: Duration = Duration::from_secs(15 * 60);
#[derive(Default)]
pub(in crate::workspace_files) struct SearchIndexes {
    entries: Mutex<HashMap<PathBuf, (Instant, Arc<Index>)>>,
    cleanup: OnceLock<()>,
}
#[derive(Default)]
struct Index {
    picker: SharedFilePicker,
    frecency: SharedFrecency,
    initialized: OnceLock<std::result::Result<bool, String>>,
}
impl Drop for Index {
    fn drop(&mut self) {
        self.picker.cancel();
        self.picker.shutdown_watches_and_wait();
        if let Ok(mut guard) = self.picker.write() {
            drop(guard.take());
        }
    }
}
impl SearchIndexes {
    pub(in crate::workspace_files) async fn search(
        self: &Arc<Self>,
        request: SearchTreeEntries,
    ) -> Result<TreeEntries> {
        request.validate().map_err(anyhow::Error::msg)?;
        self.start_cleanup();
        let owner = self.clone();
        tokio::task::spawn_blocking(move || owner.read(request)).await?
    }
    fn start_cleanup(self: &Arc<Self>) {
        self.cleanup.get_or_init(|| {
            let owner = Arc::downgrade(self);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let Some(owner) = owner.upgrade() else {
                        break;
                    };
                    // Dropping an index also stops its filesystem watcher.
                    // Do not perform that blocking cleanup on a Tokio worker.
                    let _ = tokio::task::spawn_blocking(move || owner.expire()).await;
                }
            });
        });
    }
    fn expire(&self) {
        let expired = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let roots: Vec<_> = entries
                .iter()
                .filter(|(_, (used, _))| used.elapsed() >= IDLE_TTL)
                .map(|(path, _)| path.clone())
                .collect();
            roots
                .into_iter()
                .filter_map(|root| entries.remove(&root))
                .collect::<Vec<_>>()
        };
        drop(expired);
    }
    fn read(&self, request: SearchTreeEntries) -> Result<TreeEntries> {
        let root = dunce::canonicalize(absolute_path(&request.root)?)?;
        if !root.is_dir() {
            return Err(anyhow!("作業スペースのルートがフォルダではありません。"));
        }
        let index = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let (used, index) = entries
                .entry(root.clone())
                .or_insert_with(|| (Instant::now(), Arc::default()));
            *used = Instant::now();
            index.clone()
        };
        let result = index.search(
            &root,
            &request.query,
            request.limit as usize,
            request.files_only,
        );
        if result.is_err() {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if entries
                .get(&root)
                .is_some_and(|(_, current)| Arc::ptr_eq(current, &index))
            {
                entries.remove(&root);
            }
        }
        result
    }
}

impl Index {
    fn search(
        &self,
        root: &Path,
        query: &str,
        limit: usize,
        files_only: bool,
    ) -> Result<TreeEntries> {
        let partial = self
            .initialized
            .get_or_init(|| {
                FilePicker::new_with_shared_state(
                    self.picker.clone(),
                    self.frecency.clone(),
                    FilePickerOptions {
                        base_path: root.to_str().ok_or("workspace path is not UTF-8")?.into(),
                        mode: FFFMode::Neovim,
                        enable_mmap_cache: false,
                        enable_content_indexing: false,
                        enable_fs_root_scanning: true,
                        enable_home_dir_scanning: true,
                        ..Default::default()
                    },
                )
                .map_err(|error| error.to_string())?;
                Ok(!self.picker.wait_for_scan(Duration::from_secs(15)))
            })
            .as_ref()
            .map_err(|error| anyhow!("{error}"))?;
        let guard = self.picker.read().map_err(|error| anyhow!("{error}"))?;
        let picker = guard
            .as_ref()
            .context("workspace search index is unavailable")?;
        let incomplete = *partial && picker.is_scan_active();
        let query = query
            .trim()
            .trim_start_matches(['@', '.', '/'])
            .to_lowercase();
        let options = FuzzySearchOptions {
            max_threads: 0,
            current_file: None,
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,
            pagination: PaginationArgs {
                offset: 0,
                limit: limit + 1,
            },
        };
        let (items, total_matched) = if files_only {
            let parser = QueryParser::new(FileSearchConfig);
            let parsed = parser.parse(&query);
            let result = picker.fuzzy_search(&parsed, None, options);
            (
                result
                    .items
                    .into_iter()
                    .map(MixedItemRef::File)
                    .collect::<Vec<_>>(),
                result.total_matched,
            )
        } else {
            let parser = QueryParser::new(MixedSearchConfig);
            let parsed = parser.parse(&query);
            let result = picker.fuzzy_search_mixed(&parsed, None, options);
            (result.items, result.total_matched)
        };
        let mut root_count = 0;
        let mut entries = Vec::new();
        for item in items {
            let (path, kind) = match item {
                MixedItemRef::File(file) => (file.relative_path(picker), EntryKind::File),
                MixedItemRef::Dir(directory) => {
                    (directory.relative_path(picker), EntryKind::Directory)
                }
            };
            let path = path.replace('\\', "/");
            let path = path.strip_suffix('/').unwrap_or(&path);
            if path.is_empty() {
                root_count += 1;
                continue;
            }
            if entries.len() < limit {
                entries.push(TreeEntry {
                    path: path.into(),
                    kind,
                    ignored: false,
                });
            }
        }
        Ok(TreeEntries {
            entries,
            truncated: incomplete || total_matched.saturating_sub(root_count) > limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn search_matches_path_globs_with_nested_ignore_rules() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        fs::create_dir_all(root.path().join("src/nested")).unwrap();
        fs::write(root.path().join("src/.gitignore"), "ignored.rs\n").unwrap();
        for path in [
            "src/main.rs",
            "src/nested/lib.rs",
            "src/ignored.rs",
            "src/readme.md",
        ] {
            fs::write(root.path().join(path), "content").unwrap();
        }
        let indexes = Arc::new(SearchIndexes::default());
        for files_only in [false, true] {
            let result = indexes
                .search(SearchTreeEntries {
                    root: root.path().to_str().unwrap().into(),
                    query: "src/**/*.rs".into(),
                    limit: 200,
                    files_only,
                })
                .await
                .unwrap();
            assert!(!result.truncated);
            let mut paths = result
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>();
            paths.sort_unstable();
            assert_eq!(paths, ["src/main.rs", "src/nested/lib.rs"]);
            assert!(
                result
                    .entries
                    .iter()
                    .all(|entry| entry.kind == EntryKind::File && !entry.ignored)
            );
        }
    }
    #[tokio::test]
    async fn cached_search_observes_created_and_removed_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("existing.txt"), "content").unwrap();
        let indexes = Arc::new(SearchIndexes::default());
        let request = SearchTreeEntries {
            root: root.path().to_str().unwrap().into(),
            query: "needle".into(),
            limit: 200,
            files_only: true,
        };
        assert!(
            indexes
                .search(request.clone())
                .await
                .unwrap()
                .entries
                .is_empty()
        );
        let index = indexes
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .1
            .clone();
        fs::write(root.path().join("needle.txt"), "content").unwrap();
        wait_for_search_paths(&indexes, &request, &["needle.txt"]).await;
        fs::remove_file(root.path().join("needle.txt")).unwrap();
        wait_for_search_paths(&indexes, &request, &[]).await;
        assert!(Arc::ptr_eq(
            &index,
            &indexes.entries.lock().unwrap().values().next().unwrap().1
        ));
    }
    async fn wait_for_search_paths(
        indexes: &Arc<SearchIndexes>,
        request: &SearchTreeEntries,
        expected: &[&str],
    ) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let result = indexes.search(request.clone()).await.unwrap();
                let paths = result
                    .entries
                    .iter()
                    .map(|entry| entry.path.as_str())
                    .collect::<Vec<_>>();
                if paths == expected && !result.truncated {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("filesystem changes must reach the cached search index");
    }
    #[tokio::test]
    async fn file_only_search_excludes_directories_before_ranking_and_limiting() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("needle")).unwrap();
        fs::write(root.path().join("needle/actual.txt"), "needle").unwrap();
        let indexes = Arc::new(SearchIndexes::default());
        let request = SearchTreeEntries {
            root: root.path().to_str().unwrap().into(),
            query: "needle".into(),
            limit: 1,
            files_only: false,
        };
        let mixed = indexes.search(request.clone()).await.unwrap();
        assert!(mixed.truncated);
        let files = indexes
            .search(SearchTreeEntries {
                files_only: true,
                ..request.clone()
            })
            .await
            .unwrap();
        assert_eq!(
            files.entries,
            [TreeEntry {
                path: "needle/actual.txt".into(),
                kind: EntryKind::File,
                ignored: false,
            }]
        );
        assert!(
            !files.truncated,
            "excluded directories must not exhaust the file limit"
        );
        let folder_query = SearchTreeEntries {
            query: "needle/".into(),
            ..request
        };
        let folders = indexes.search(folder_query.clone()).await.unwrap();
        assert_eq!(folders.entries[0].kind, EntryKind::Directory);
        let files = indexes
            .search(SearchTreeEntries {
                files_only: true,
                ..folder_query
            })
            .await
            .unwrap();
        assert_eq!(files.entries[0].path, "needle/actual.txt");
        assert_eq!(files.entries[0].kind, EntryKind::File);
        assert!(!files.truncated);
    }
    #[tokio::test]
    async fn search_uses_t3s_fuzzy_engine_and_normalization_without_ignored_files() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir(root.path().join("ignored")).unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.path().join("src/manifest.rs"), "// manifest").unwrap();
        fs::write(root.path().join("ignored/main.rs"), "ignored").unwrap();
        let indexes = Arc::new(SearchIndexes::default());
        let request = SearchTreeEntries {
            root: root.path().to_str().unwrap().into(),
            query: "@./MAIN.RS ".into(),
            limit: 200,
            files_only: false,
        };
        let result = indexes.search(request.clone()).await.unwrap();
        assert!(!result.truncated);
        assert_eq!(
            result.entries[0],
            TreeEntry {
                path: "src/main.rs".into(),
                kind: EntryKind::File,
                ignored: false
            }
        );
        assert!(
            !result
                .entries
                .iter()
                .any(|entry| entry.path.starts_with("ignored/"))
        );
        let folders = indexes
            .search(SearchTreeEntries {
                query: "src".into(),
                ..request.clone()
            })
            .await
            .unwrap();
        assert!(
            folders
                .entries
                .iter()
                .any(|entry| entry.path == "src" && entry.kind == EntryKind::Directory)
        );
        let limited = indexes
            .search(SearchTreeEntries {
                query: "rs".into(),
                limit: 1,
                ..request
            })
            .await
            .unwrap();
        assert_eq!(limited.entries.len(), 1);
        assert!(limited.truncated);
    }
}
