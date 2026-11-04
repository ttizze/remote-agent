use super::*;
use std::time::{Duration, SystemTime};

/// A remote file remains private until its current view accepts the download.
/// Rejected/stale completions drop the directory instead of launching an app.
pub(in crate::app) struct DownloadedPreview {
    directory: tempfile::TempDir,
    path: PathBuf,
}

impl DownloadedPreview {
    fn into_url(self) -> Result<String, String> {
        let url = file_url(&self.path)?;
        // The receiving OS application may reopen the file after Bex closes.
        // The preview cache retains accepted exports for a day.
        let _ = self.directory.keep();
        Ok(url)
    }
}

fn file_url(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(String::from)
        .map_err(|_| "ファイルを開けません".into())
}

fn preview_directory(root: &Path, now: SystemTime) -> std::io::Result<tempfile::TempDir> {
    std::fs::create_dir_all(root)?;
    let root = std::fs::canonicalize(root)?;
    for entry in std::fs::read_dir(&root)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with("preview-")
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified)
                        .is_ok_and(|age| age > Duration::from_secs(24 * 60 * 60))
                })
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    tempfile::Builder::new().prefix("preview-").tempdir_in(root)
}

async fn download_preview(
    store: &Store,
    source: String,
    root: PathBuf,
) -> Result<DownloadedPreview, String> {
    let directory =
        tokio::task::spawn_blocking(move || preview_directory(&root, SystemTime::now()))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
    let name = agent_core::presentation::markdown::file_basename(&source);
    let path = directory.path().join(name);
    download_to(store, source, path.clone()).await?;
    Ok(DownloadedPreview { directory, path })
}

impl Desktop {
    pub(in crate::app) fn preview_file(&mut self, source: String, cx: &mut Context<Self>) {
        if self.remote.is_none() {
            match file_url(Path::new(&source)) {
                Ok(url) => cx.open_url(&url),
                Err(error) => self.set_error(error),
            }
            cx.notify();
            return;
        }
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
            self.set_error("ホストに接続していません".into());
            cx.notify();
            return;
        };
        let generation = self.file_open_generation;
        let scope = self.local_error_scope();
        self.busy += 1;
        self.effect(
            async move {
                let root = platform::state_dir()?.join("file-previews");
                download_preview(&store, source, root).await
            },
            move |result| Update::FilePreview {
                generation,
                scope,
                result,
            },
        );
    }

    pub(in crate::app) fn complete_file_preview(
        &mut self,
        generation: u64,
        scope: ErrorScope,
        result: Result<DownloadedPreview, String>,
        cx: &mut Context<Self>,
    ) {
        self.busy = self.busy.saturating_sub(1);
        if generation != self.file_open_generation || scope != self.local_error_scope() {
            return;
        }
        match result.and_then(DownloadedPreview::into_url) {
            Ok(url) => cx.open_url(&url),
            Err(error) => {
                self.local_errors.insert(scope, error);
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[gpui::test]
    fn only_current_downloads_launch_and_keep_their_files(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let (desktop, cx) = cx.add_window_view(|window, cx| {
            Desktop::new(
                Mode::SideChat {
                    remote: None,
                    cwd: root.path().to_str().unwrap().into(),
                },
                window,
                cx,
            )
        });
        for (generation, stale_scope, accepted) in
            [(1, false, false), (2, true, false), (2, false, true)]
        {
            let directory = preview_directory(root.path(), SystemTime::now()).unwrap();
            let path = directory.path().join("movie.mp4");
            std::fs::write(&path, b"downloaded bytes").unwrap();
            cx.update(|_, cx| {
                desktop.update(cx, |view, cx| {
                    view.file_open_generation = 2;
                    view.busy = 1;
                    let scope = if stale_scope {
                        ErrorScope::Application
                    } else {
                        view.local_error_scope()
                    };
                    view.complete_file_preview(
                        generation,
                        scope,
                        Ok(DownloadedPreview {
                            directory,
                            path: path.clone(),
                        }),
                        cx,
                    );
                    assert_eq!(view.busy, 0);
                })
            });
            assert_eq!(path.exists(), accepted);
            assert_eq!(cx.opened_url(), accepted.then(|| file_url(&path).unwrap()));
        }
    }

    #[test]
    fn native_urls_preserve_names_and_exports_outlive_the_view() {
        let root = tempfile::tempdir().unwrap();
        let directory = preview_directory(root.path(), SystemTime::now()).unwrap();
        let path = directory.path().join("動画 #1?.mp4");
        std::fs::write(&path, b"movie").unwrap();
        let url = DownloadedPreview {
            directory,
            path: path.clone(),
        }
        .into_url()
        .unwrap();
        assert_eq!(url::Url::parse(&url).unwrap().to_file_path().unwrap(), path);
        assert_eq!(std::fs::read(path).unwrap(), b"movie");
    }

    #[tokio::test]
    async fn failed_downloads_remove_their_private_preview_directory() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::offline(Snapshot::default());
        assert!(
            download_preview(&store, "/movie.mp4".into(), root.path().into())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn cleanup_keeps_recent_previews_and_unrelated_directories() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("preview-old");
        let unrelated = root.path().join("other-files");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        let yesterday = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        let times = std::fs::FileTimes::new().set_modified(yesterday);
        std::fs::File::open(&old).unwrap().set_times(times).unwrap();
        std::fs::File::open(&unrelated)
            .unwrap()
            .set_times(times)
            .unwrap();
        let recent = preview_directory(root.path(), SystemTime::now()).unwrap();
        let another = preview_directory(root.path(), SystemTime::now()).unwrap();
        assert!(!old.exists());
        assert!(unrelated.exists());
        assert!(recent.path().exists());
        assert!(another.path().exists());
    }
}
