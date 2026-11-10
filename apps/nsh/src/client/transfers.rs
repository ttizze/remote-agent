use super::Client;
use crate::storage::error;
use agent_core::{
    client::AgentError,
    state::{Attachment, DraftKey, Intent, operations::UploadAttachment},
};
use std::{path::PathBuf, sync::Arc};

pub(crate) struct LocalFile {
    _directory: tempfile::TempDir,
    pub path: PathBuf,
}

impl LocalFile {
    #[cfg(target_os = "ios")]
    pub fn markdown(filename: String, text: String) -> Result<Self, std::io::Error> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(filename);
        std::fs::write(&path, text)?;
        Ok(Self {
            _directory: directory,
            path,
        })
    }

    fn create(root: PathBuf, filename: &str) -> Result<Self, AgentError> {
        agent_local_host::create_state_directory(&root).map_err(error)?;
        let directory = tempfile::tempdir_in(root).map_err(error)?;
        Ok(Self {
            path: directory.path().join(filename),
            _directory: directory,
        })
    }
}

pub(crate) enum AttachmentSource {
    File { path: PathBuf, is_image: bool },
    Image { name: String, bytes: Vec<u8> },
}

impl Client {
    pub fn download(
        self: &Arc<Self>,
        host: String,
        source: String,
    ) -> async_channel::Receiver<Result<Arc<LocalFile>, AgentError>> {
        let (send, receive) = async_channel::bounded(1);
        let owner = self.clone();
        self.spawn(async move {
            let result = owner.download_file(&host, source).await;
            // If the view went away, dropping the result releases only this transfer's directory.
            let _ = send.send(result).await;
        });
        receive
    }

    async fn download_file(
        &self,
        host: &str,
        source: String,
    ) -> Result<Arc<LocalFile>, AgentError> {
        let root = self.root.join("downloads");
        let filename = source
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !["", ".", ".."].contains(name))
            .unwrap_or("file")
            .to_owned();
        let file = tokio::task::spawn_blocking(move || LocalFile::create(root, &filename))
            .await
            .map_err(error)??;
        self.execute(
            host,
            None,
            Intent::DownloadFile(agent_core::state::operations::DownloadFile {
                source,
                destination: file.path.to_string_lossy().into_owned(),
            }),
        )?
        .wait()
        .await?;
        Ok(Arc::new(file))
    }

    pub fn cache_image(
        self: &Arc<Self>,
        bytes: Vec<u8>,
        extension: String,
    ) -> async_channel::Receiver<Result<Arc<LocalFile>, AgentError>> {
        let (send, receive) = async_channel::bounded(1);
        let root = self.root.join("downloads");
        self.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let file = LocalFile::create(root, &format!("image.{extension}"))?;
                agent_local_host::save_private_bytes(&file.path, &bytes).map_err(error)?;
                Ok::<_, AgentError>(Arc::new(file))
            })
            .await
            .map_err(error)
            .and_then(|result| result);
            let _ = send.send(result).await;
        });
        receive
    }

    /// A selected batch stays owned here while its originating view changes.
    pub fn attach_batch(
        self: &Arc<Self>,
        id: uuid::Uuid,
        host: String,
        pane: Option<u64>,
        draft_key: DraftKey,
        sources: Vec<AttachmentSource>,
    ) {
        let owner = self.clone();
        self.spawn(async move {
            for source in sources {
                match owner
                    .upload_source(&host, pane, draft_key.clone(), source)
                    .await
                {
                    Ok(true) => {}
                    // Retain core's failure notice; a later upload would replace it.
                    Ok(false) => break,
                    Err(reason) => {
                        owner.report(reason);
                        break;
                    }
                }
            }
            let _ = owner
                .updates
                .send(super::Update::AttachmentsFinished(id))
                .await;
        });
    }

    async fn upload_source(
        &self,
        host: &str,
        pane: Option<u64>,
        draft_key: DraftKey,
        source: AttachmentSource,
    ) -> Result<bool, AgentError> {
        let (attachment, local) = match source {
            AttachmentSource::File { path, is_image } => (
                Attachment {
                    name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path: path.to_string_lossy().into_owned(),
                    is_image,
                },
                None,
            ),
            AttachmentSource::Image { name, bytes } => {
                let root = self.root.join("uploads");
                let file = tokio::task::spawn_blocking(move || {
                    let file = LocalFile::create(root, "image")?;
                    agent_local_host::save_private_bytes(&file.path, &bytes).map_err(error)?;
                    Ok::<_, AgentError>(file)
                })
                .await
                .map_err(error)??;
                (
                    Attachment {
                        path: file.path.to_string_lossy().into_owned(),
                        name,
                        is_image: true,
                    },
                    Some(file),
                )
            }
        };
        let receipt = self.execute(
            host,
            pane,
            Intent::UploadAttachment(UploadAttachment {
                draft_key,
                attachment,
            }),
        )?;
        // Core publishes operation errors; only staging/dispatch failures are reported above.
        let uploaded = receipt.wait().await.is_ok();
        drop(local);
        Ok(uploaded)
    }
}
