use super::*;
use agent_protocol::models::{ProjectRoot, SCRATCH_PROJECT_ID};

const DIRECTORY: &str = "scratch";

impl ProjectStore {
    pub(crate) fn scratch_directory(&self) -> PathBuf {
        self.path.with_file_name(DIRECTORY)
    }

    /// Resolve configured and physical roots even before Scratch exists.
    pub(crate) async fn scratch_roots(
        &self,
    ) -> anyhow::Result<std::collections::BTreeSet<PathBuf>> {
        let configured = self.scratch_directory();
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_owned();
        Ok(tokio::task::spawn_blocking(move || -> io::Result<_> {
            let mut roots = std::collections::BTreeSet::from([configured.clone()]);
            for path in [&parent, &configured] {
                match std::fs::canonicalize(path) {
                    Ok(physical) => {
                        let physical = dunce::simplified(&physical);
                        roots.insert(if path == &configured {
                            physical.to_owned()
                        } else {
                            physical.join(DIRECTORY)
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(roots)
        })
        .await??)
    }

    pub(crate) async fn ensure_scratch(&self) -> anyhow::Result<PathBuf> {
        let directory = self.scratch_directory();
        tokio::fs::create_dir_all(&directory).await?;
        let directory = tokio::fs::canonicalize(directory).await?;
        let directory = dunce::simplified(&directory).to_owned();
        let root = directory.clone();
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || {
            anyhow::ensure!(
                crate::git::text(&root, &["rev-parse", "--show-toplevel"]).is_err(),
                "Threads without a project require a data directory outside Git repositories."
            );
            storage
                .register_project(&Project {
                    id: SCRATCH_PROJECT_ID.into(),
                    name: "No project".into(),
                    roots: vec![ProjectRoot {
                        path: root.to_str().context("workspace path is not UTF-8")?.into(),
                    }],
                    icon: None,
                })
                .map_err(anyhow::Error::msg)
        })
        .await??;
        Ok(directory)
    }

    pub(crate) async fn create_chat_directory(&self, text: &str) -> anyhow::Result<PathBuf> {
        let root = self.ensure_scratch().await?;
        let date: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let folder = folder_name(
            &date.format("%Y-%m-%d").to_string(),
            text,
            uuid::Uuid::new_v4(),
        );
        let directory = root.join(folder);
        // Claim a fresh folder; never reuse another conversation's files.
        tokio::fs::create_dir(&directory).await?;
        Ok(directory)
    }

    /// Native history can outlive the application's data folder. Restore its
    /// recorded working directory before resuming, without claiming new files.
    pub(crate) async fn restore_chat_directory(
        &self,
        directory: &Path,
    ) -> anyhow::Result<Option<bool>> {
        let roots = self.scratch_roots().await?;
        if state::scratch_root(&roots, directory).is_none() {
            return Ok(None);
        }
        match tokio::fs::metadata(directory).await {
            Ok(metadata) => {
                anyhow::ensure!(metadata.is_dir(), "workspace path is not a directory");
                Ok(Some(false))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.ensure_scratch().await?;
                tokio::fs::create_dir_all(directory).await?;
                Ok(Some(true))
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn folder_name(date: &str, text: &str, id: uuid::Uuid) -> String {
    let words = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(48)
        .collect::<String>()
        .to_lowercase();
    [date, &words, &id.to_string()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn conversations_claim_separate_folders_in_one_durable_project() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::fixture(root.path().join("projects.json"));
        let (one, two) = tokio::join!(
            store.create_chat_directory("Convert these PNGs to WebP"),
            store.create_chat_directory("Convert these PNGs to WebP"),
        );
        let (one, two) = (one.unwrap(), two.unwrap());
        assert_ne!(one, two);
        assert_eq!(one.parent(), two.parent());
        assert!(
            one.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains("convert-these-pngs-to-webp")
        );
        std::fs::write(one.join("result.txt"), "first").unwrap();
        std::fs::write(two.join("result.txt"), "second").unwrap();
        assert_eq!(
            std::fs::read_to_string(one.join("result.txt")).unwrap(),
            "first"
        );
        let snapshot = ProjectStore::fixture(store.path()).load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(
            snapshot.project_for_workspace(one.to_str().unwrap()),
            Some(SCRATCH_PROJECT_ID)
        );
        assert_eq!(
            snapshot.project_for_workspace(two.to_str().unwrap()),
            Some(SCRATCH_PROJECT_ID)
        );
    }

    #[tokio::test]
    async fn scratch_does_not_inherit_a_repository_from_its_parent() {
        let directory = tempfile::tempdir().unwrap();
        crate::git::text(directory.path(), &["init", "--quiet"]).unwrap();
        let store = ProjectStore::fixture(directory.path().join("projects.json"));
        assert!(store.create_chat_directory("test").await.is_err());
        assert!(store.storage.projects().unwrap().is_empty());
    }

    proptest::proptest! {
        #[test]
        fn names_are_single_bounded_components(text in ".{0,1000}") {
            let id = uuid::Uuid::nil();
            let name = folder_name("2026-11-17", &text, id);
            proptest::prop_assert_eq!(Path::new(&name).components().count(), 1);
            proptest::prop_assert!(name.starts_with("2026-11-17-"));
            proptest::prop_assert!(name.ends_with(&id.to_string()));
            proptest::prop_assert!(name.len() <= 255);
        }
    }
}
