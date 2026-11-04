use super::*;
use agent_core::{presentation::error::error_text, store::Store};
mod browser;
mod explorer;
pub(super) use browser::{FileBrowser, FileBrowserModel};
pub(super) use explorer::FileExplorer;
mod menu;
mod preview;
mod reference;
pub(super) use menu::run_file_menu_command;
pub(super) use preview::DownloadedPreview;
pub(super) use reference::{ReferenceAction, ReferenceRequest};

impl Desktop {
    pub(super) fn open_in_editor(
        &mut self,
        path: String,
        line: Option<u32>,
        column: Option<u32>,
        editor: agent_protocol::editors::EditorId,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.local_shell {
            return;
        }
        if !self.snapshot.connected {
            self.set_error("ホストに接続していません".into());
            cx.notify();
            return;
        }
        self.busy += 1;
        self.perform(
            Intent::OpenInEditor(op::OpenInEditor {
                path,
                line,
                column,
                editor,
                reveal,
            }),
            OperationCompletion::Busy,
        );
        cx.notify();
    }
    pub(super) fn download(&mut self, source: String) {
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
            self.set_error("ホストに接続していません".into());
            return;
        };
        self.busy += 1;
        let scope = self.local_error_scope();
        self.effect(
            async move {
                let name = source
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&source)
                    .to_owned();
                let destination =
                    tokio::task::spawn_blocking(move || platform::choose_destination(&name))
                        .await
                        .map_err(|error| format!("保存先を選択できません: {error}"))?;
                if let Some(destination) = destination {
                    download_to(&store, source, destination).await?;
                }
                Ok(())
            },
            move |result| Update::LocalCompleted { scope, result },
        );
    }
}

pub(super) async fn download_to(
    store: &Store,
    source: String,
    destination: PathBuf,
) -> Result<(), String> {
    store
        .dispatch(Intent::DownloadFile(op::DownloadFile {
            source,
            destination: destination
                .into_os_string()
                .into_string()
                .map_err(|_| "保存先のパスを読み取れません")?,
        }))
        .await
        .map_err(|error| {
            format!(
                "ファイルを取得・保存できません: {}",
                error_text(&error.to_string())
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_explicit_downloads_preserve_existing_files_and_explain_the_failure() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("saved.zip");
        std::fs::write(&destination, b"existing file").unwrap();
        let store = Store::offline(Snapshot::default());
        let error = download_to(&store, "/bundle.zip".into(), destination.clone())
            .await
            .unwrap_err();
        assert!(error.starts_with("ファイルを取得・保存できません: "));
        assert!(error.len() > "ファイルを取得・保存できません: ".len());
        assert_eq!(std::fs::read(destination).unwrap(), b"existing file");
    }
}
