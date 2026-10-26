//! Chrome bookmark migration; login state belongs to the persistent WebView profile.
use super::*;
use agent_core::browser_import::{self, BrowserBookmark, MAX_IMPORT_BYTES};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
fn data_path() -> Result<PathBuf, String> {
    directories::ProjectDirs::from("app", "bex", "BEX")
        .map(|dirs| dirs.data_local_dir().join("browser-bookmarks.json"))
        .ok_or_else(|| "ブラウザの保存先を取得できません".into())
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "ファイルを開けません")?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "ファイルを読み取れません")?;
    if bytes.len() > limit {
        return Err("ファイルが大きすぎます".into());
    }
    Ok(bytes)
}
fn load(path: &Path) -> Result<Vec<BrowserBookmark>, String> {
    match fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("ブックマークを確認できません".into()),
        Ok(_) => {}
    }
    serde_json::from_slice(&bounded_read(path, MAX_IMPORT_BYTES * 4)?)
        .map_err(|_| "ブックマークを読み取れません".into())
}
/// None clears the collection; an import merges against the latest locked contents.
fn update(path: &Path, imported: Option<Vec<BrowserBookmark>>) -> Result<(), String> {
    let parent = path.parent().ok_or("保存先が正しくありません")?;
    fs::create_dir_all(parent).map_err(|_| "保存フォルダを作成できません")?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|_| "ブックマークをロックできません")?;
    lock.lock().map_err(|_| "ブックマークをロックできません")?;
    let previous = load(path)?;
    let bookmarks = if let Some(imported) = imported {
        browser_import::merge_bookmarks(previous, imported)?
    } else {
        Vec::new()
    };
    let bytes = serde_json::to_vec(&bookmarks).map_err(|_| "ブックマークを保存できません")?;
    if bytes.len() > MAX_IMPORT_BYTES * 4 {
        return Err("保存データが大きすぎます".into());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "保存ファイルを作成できません")?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| "ブックマークを保存できません")?;
    temporary
        .persist(path)
        .map_err(|_| "ブックマークを置き換えられません")?;
    Ok(())
}
fn chrome_profiles() -> Result<Vec<(String, PathBuf)>, String> {
    let root = directories::BaseDirs::new()
        .ok_or("Chromeの保存先を取得できません")?
        .data_local_dir()
        .join("Google/Chrome/User Data");
    let state = bounded_read(&root.join("Local State"), MAX_IMPORT_BYTES)?;
    let result: Vec<_> = browser_import::parse_chrome_profiles(&state)?
        .into_iter()
        .filter_map(|profile| {
            let path = root.join(&profile.directory).join("Bookmarks");
            path.is_file()
                .then(|| (format!("{} ({})", profile.name, profile.directory), path))
        })
        .collect();
    if result.is_empty() {
        return Err("ブックマークのあるChromeプロファイルがありません".into());
    }
    Ok(result)
}
impl BrowserPage {
    fn run_import(
        &mut self,
        work: impl FnOnce() -> Result<String, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.importing {
            return;
        }
        self.importing = true;
        self.error.clear();
        self.import_notice = "取り込み中…".into();
        let task = cx.background_executor().spawn(async move { work() });
        cx.spawn(async move |owner, cx| {
            let result = task.await;
            let _ = owner.update(cx, |s, cx| {
                s.importing = false;
                match result {
                    Ok(notice) => s.import_notice = notice,
                    Err(error) => {
                        s.import_notice.clear();
                        s.error = error;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn windows_import_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let import_owner = owner.clone();
        let visibility = owner.clone();
        let bookmarks_owner = owner.clone();
        h_flex().px_2().pb_2().gap_1()
            .child(Button::new("browser-import").label("Chromeのブックマークを取り込む").small().ghost().disabled(self.importing).dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    match chrome_profiles() {
                        Ok(profiles) => for (name, path) in profiles {
                            let owner = import_owner.clone();
                            menu = menu.item(PopupMenuItem::new(name).on_click(move |_, _, cx| {
                                let path = path.clone();
                                let _ = owner.update(cx, |browser, cx| browser.run_import(move || {
                                    let bookmarks = browser_import::parse_chrome_bookmarks(&bounded_read(&path, MAX_IMPORT_BYTES)?)?;
                                    let count = bookmarks.len();
                                    update(&data_path()?, Some(bookmarks))?;
                                    Ok(format!("ブックマーク{count}件を取り込みました。"))
                                }, cx));
                            }));
                        },
                        Err(error) => menu = menu.item(PopupMenuItem::new(error).disabled(true)),
                    }
                    menu.item(PopupMenuItem::new("Chromeのログイン状態の取り込みは非対応です").disabled(true))
                }).on_open_change(move |open, _, cx| { let _ = visibility.update(cx, |browser, cx| { browser.profile_menu_open = *open; browser.set_visible(browser.visible, cx); cx.notify(); }); }))
            .child(Button::new("browser-bookmarks").label("ブックマーク").small().ghost().dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    match data_path().and_then(|path| load(&path)) {
                        Ok(bookmarks) if !bookmarks.is_empty() => {
                            for bookmark in bookmarks {
                                let owner = bookmarks_owner.clone();
                                menu = menu.item(PopupMenuItem::new(bookmark.name).on_click(move |_, window, cx| { let _ = owner.update(cx, |browser, cx| browser.open_url(&bookmark.url, window, cx)); }));
                            }
                            let owner = bookmarks_owner.clone();
                            menu = menu.item(PopupMenuItem::new("取り込んだブックマークを削除").on_click(move |_, _, cx| { let _ = owner.update(cx, |browser, cx| browser.run_import(|| { update(&data_path()?, None)?; Ok("取り込んだブックマークを削除しました。".into()) }, cx)); }));
                        },
                        Ok(_) => menu = menu.item(PopupMenuItem::new("取り込んだブックマークはありません").disabled(true)),
                        Err(error) => menu = menu.item(PopupMenuItem::new(error).disabled(true)),
                    }
                    menu
                }).on_open_change(move |open, _, cx| { let _ = owner.update(cx, |browser, cx| { browser.profile_menu_open = *open; browser.set_visible(browser.visible, cx); cx.notify(); }); }))
    }
}
#[cfg(test)]
mod tests {
    use super::{BrowserBookmark, fs, load, update};
    #[test]
    fn imports_merge_reopen_and_preserve_corrupt_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bookmarks.json");
        update(
            &path,
            Some(vec![BrowserBookmark {
                name: "Old".into(),
                url: "https://example.test/".into(),
            }]),
        )
        .unwrap();
        update(
            &path,
            Some(vec![BrowserBookmark {
                name: "New".into(),
                url: "https://example.test/".into(),
            }]),
        )
        .unwrap();
        assert_eq!(load(&path).unwrap()[0].name, "New");
        update(&path, None).unwrap();
        assert!(load(&path).unwrap().is_empty());
        fs::write(&path, b"corrupt").unwrap();
        assert!(update(&path, Some(vec![])).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"corrupt");
    }
}
