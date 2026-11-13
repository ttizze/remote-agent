//! Explicit file-reference actions. Hover metadata never redirects a path.
use super::*;
use crate::presentation::{
    files::file_tree_path,
    markdown::{
        MarkdownFileSurface, WORKSPACE_BASENAME_LOOKUP_LIMIT, file_host_path,
        needs_workspace_basename_lookup, pick_stripped_workspace_match,
        pick_workspace_basename_match, strip_repeated_workspace_prefix,
    },
};
use agent_protocol::{file_metadata::ReadFileMetadata, file_tree::SearchTreeEntries};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ResolveFileReference {
    pub path: String,
    pub root: String,
    pub lookup_path: Option<String>,
    pub surface: MarkdownFileSurface,
    /// Only panel opens need to distinguish a directory from a file. Editor
    /// launches and path copies must also work for targets that do not exist.
    pub inspect_directory: bool,
}

impl Operation for ResolveFileReference {
    type Input = Option<String>;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        Ok(snapshot.connection.host_home_directory.clone())
    }
    type Output = (String, bool);
    fn prepare(&mut self, _: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        let path = &self.path;
        if !path.contains('\0')
            && (agent_protocol::paths::is_absolute_host_path(path)
                || path == "~"
                || path.starts_with("~/")
                || path.starts_with("~\\"))
        {
            Ok(())
        } else {
            Err("ホスト上の絶対パスが必要です。".into())
        }
    }
    async fn run(
        &self,
        home: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let root = file_host_path(self.root.clone(), home.clone());
        let literal = file_host_path(self.path.clone(), home);
        let path = workspace_match(
            context,
            &root,
            self.lookup_path.as_deref(),
            &literal,
            self.surface,
        )
        .await
        .unwrap_or(literal);
        if self.inspect_directory
            && let Ok(entry) = context.call(&rpc::ResolveFile { path: path.clone() }).await
        {
            return Ok((entry.path, entry.directory));
        }
        // A missing/unreadable target still opens its file panel, which owns
        // the read error and explicit download action.
        Ok((path, false))
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::FileReferenceResolved {
            path: output.0.clone(),
            directory: output.1,
        }
    }
}

async fn workspace_match(
    context: &mut Execution<'_>,
    root: &str,
    lookup: Option<&str>,
    literal: &str,
    surface: MarkdownFileSurface,
) -> Option<String> {
    let lookup = lookup?;
    let basename = needs_workspace_basename_lookup(lookup);
    let stripped = (!basename)
        .then(|| strip_repeated_workspace_prefix(lookup, root))
        .flatten();
    let query = if basename {
        lookup
    } else {
        stripped.as_deref()?
    };
    let request = SearchTreeEntries {
        root: root.into(),
        query: query.into(),
        limit: WORKSPACE_BASENAME_LOOKUP_LIMIT,
        files_only: true,
    };
    request.validate().ok()?;
    let entries = context.call(&request).await.ok()?.entries;
    let matched = if basename {
        pick_workspace_basename_match(lookup, &entries)
    } else {
        pick_stripped_workspace_match(query, root, &entries)
    }?;
    let protect_directory = surface == MarkdownFileSurface::Mobile && basename && matched != lookup;
    if stripped.is_some() || protect_directory {
        // FFF may still be scanning or truncated: only a Host probe can rule
        // out the literal path before redirecting to the stripped candidate.
        let request = ReadFileMetadata {
            paths: vec![literal.into()],
        };
        request.validate().ok()?;
        let metadata = context.call(&request).await.ok()?;
        let metadata = metadata.entries.first()?.metadata.as_ref();
        if (stripped.is_some() && metadata.is_some())
            || metadata.is_some_and(|metadata| {
                protect_directory
                    && metadata.kind == agent_protocol::file_metadata::FileMetadataKind::Directory
            })
        {
            return None;
        }
    }
    Some(file_tree_path(root, &matched))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use serde_json::json;
    use std::time::Duration;

    use crate::test_host as host_fixture;

    #[test]
    fn explicit_reference_actions_require_absolute_or_home_paths_without_nul() {
        let mut snapshot = Snapshot::default();
        for path in [
            "/file",
            "~/file",
            r"C:\file",
            r"~\file",
            r"\\server\share\file",
        ] {
            let mut request = ResolveFileReference {
                surface: MarkdownFileSurface::Desktop,
                path: path.into(),
                root: String::new(),
                lookup_path: None,
                inspect_directory: false,
            };
            assert!(request.prepare(&mut snapshot, "").is_ok(), "{path}");
        }
        for path in ["", "relative.rs", "~someone/file", "/file\0"] {
            let mut request = ResolveFileReference {
                surface: MarkdownFileSurface::Desktop,
                path: path.into(),
                root: String::new(),
                lookup_path: None,
                inspect_directory: false,
            };
            assert!(request.prepare(&mut snapshot, "").is_err(), "{path}");
        }
        assert_eq!(
            snapshot,
            Snapshot::default(),
            "validation never changes client state"
        );
    }

    async fn fixture() -> (Store, host_fixture::Reader, host_fixture::Writer) {
        let initial = Snapshot {
            connection: crate::state::Connection {
                host_home_directory: Some("/actual/home".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
        let store = Store::new(peer, initial, None);
        for _ in 0..3 {
            let request = reader.read_request().await.unwrap().unwrap();
            let result = match request["method"].as_str().unwrap() {
                "host/list/subscribe" => {
                    json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5})
                }
                "host/model/list" => json!({"data":[],"nextCursor":null}),
                "host/account/list" => json!({"accounts":[],"selected":{}}),
                method => panic!("unexpected bootstrap: {method}"),
            };
            writer
                .reply(&request, json!({"result": result}))
                .await
                .unwrap();
        }
        (store, reader, writer)
    }

    #[tokio::test]
    async fn basename_actions_choose_the_first_exact_file_and_inspect_only_panel_targets() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (store, mut reader, writer) = fixture().await;
            let result = store.dispatch(Intent::ResolveFileReference(ResolveFileReference { surface: MarkdownFileSurface::Desktop,
                path: "/workspace/main.rs".into(), root: "/workspace".into(), lookup_path: Some("main.rs".into()), inspect_directory: false,
            }));
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/file/search");
            assert_eq!(request["params"], json!({"root":"/workspace","query":"main.rs","limit":25,"files_only":true}));
            writer.reply(&request, json!({"result":{"entries":[
                {"path":"main.rs","kind":"directory","ignored":false},
                {"path":"a/main.rs","kind":"file","ignored":false},
                {"path":"b/main.rs","kind":"file","ignored":false}
            ],"truncated":true}})).await.unwrap();
            assert_eq!(result.await.unwrap(), Outcome::FileReferenceResolved { path: "/workspace/a/main.rs".into(), directory: false });

            let result = store.dispatch(Intent::ResolveFileReference(ResolveFileReference { surface: MarkdownFileSurface::Desktop,
                path: "~/Documents".into(), root: String::new(), lookup_path: None, inspect_directory: true,
            }));
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/file/resolve");
            assert_eq!(request["params"]["path"], "/actual/home/Documents");
            writer.reply(&request, json!({"result":{"name":"Documents","path":"/actual/home/Documents","directory":true,"size":4096}})).await.unwrap();
            assert_eq!(result.await.unwrap(), Outcome::FileReferenceResolved { path: "/actual/home/Documents".into(), directory: true });
            store.close().await.unwrap();
        }).await.expect("reference action deadline");
    }

    #[tokio::test]
    async fn repeated_prefixes_redirect_only_after_the_literal_host_path_is_missing() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (store, mut reader, writer) = fixture().await;
            for exists in [Some(true), Some(false), None] {
                let literal = "/workspace/repo/repo/src/main.rs";
                let result = store.dispatch(Intent::ResolveFileReference(ResolveFileReference { surface: MarkdownFileSurface::Desktop,
                    path: literal.into(), root: "/workspace/repo".into(), lookup_path: Some("repo/src/main.rs".into()), inspect_directory: false,
                }));
                let request = reader.read_request().await.unwrap().unwrap();
                assert_eq!(request["method"], "host/file/search");
                assert_eq!(request["params"]["query"], "src/main.rs");
                writer.reply(&request, json!({"result":{"entries":[{"path":"src/main.rs","kind":"file","ignored":false}],"truncated":true}})).await.unwrap();
                let probe = reader.read_request().await.unwrap().unwrap();
                assert_eq!(probe["method"], "host/file/metadata");
                assert_eq!(probe["params"]["paths"], json!([literal]));
                let reply = match exists {
                    Some(exists) => json!({"result":{"entries":[{"path":literal,"metadata":exists.then(|| json!({"kind":"file","byte_length":1,"mime_type":null}))}]}}),
                    None => json!({"error":{"code":"unavailable","message":"probe failed","delivery":"notSent"}}),
                };
                writer.reply(&probe, reply).await.unwrap();
                assert_eq!(result.await.unwrap(), Outcome::FileReferenceResolved {
                    path: if exists == Some(false) { "/workspace/repo/src/main.rs" } else { literal }.into(), directory: false,
                });
            }
            assert!(store.snapshot().panes.primary().error.is_none());
            store.close().await.unwrap();
        }).await.expect("literal path probe deadline");
    }

    #[tokio::test]
    async fn failed_search_and_directory_reads_leave_the_literal_file_panel_available() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (store, mut reader, writer) = fixture().await;
            let result = store.dispatch(Intent::ResolveFileReference(ResolveFileReference { surface: MarkdownFileSurface::Desktop,
                path: "/workspace/missing.rs".into(), root: "/workspace".into(), lookup_path: Some("missing.rs".into()), inspect_directory: true,
            }));
            for method in ["host/file/search", "host/file/resolve"] {
                let request = reader.read_request().await.unwrap().unwrap();
                assert_eq!(request["method"], method);
                writer.reply(&request, json!({"error":{"code":"unavailable","message":"not found","delivery":"notSent"}})).await.unwrap();
            }
            assert_eq!(result.await.unwrap(), Outcome::FileReferenceResolved { path: "/workspace/missing.rs".into(), directory: false });
            assert!(store.snapshot().panes.primary().error.is_none());
            store.close().await.unwrap();
        }).await.expect("failed reference lookup deadline");
    }
    #[tokio::test]
    async fn mobile_basename_lookup_keeps_literal_directories_and_failed_probes() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (store, mut reader, writer) = fixture().await;
            for kind in ["directory", "file", "missing", "unavailable"] {
                let result = store.dispatch(Intent::ResolveFileReference(ResolveFileReference {
                    surface: MarkdownFileSurface::Mobile, path: "/workspace/main.rs".into(),
                    root: "/workspace".into(), lookup_path: Some("main.rs".into()), inspect_directory: true,
                }));
                let search = reader.read_request().await.unwrap().unwrap();
                assert_eq!(search["method"], "host/file/search");
                writer.reply(&search, json!({"result":{"entries":[{"path":"nested/main.rs","kind":"file","ignored":false}],"truncated":false}})).await.unwrap();
                let probe = reader.read_request().await.unwrap().unwrap();
                assert_eq!(probe["method"], "host/file/metadata");
                assert_eq!(probe["params"]["paths"], json!(["/workspace/main.rs"]));
                let response = match kind {
                    "unavailable" => json!({"error":{"code":"unavailable","message":"probe failed","delivery":"notSent"}}),
                    "missing" => json!({"result":{"entries":[{"path":"/workspace/main.rs","metadata":null}]}}),
                    _ => json!({"result":{"entries":[{"path":"/workspace/main.rs","metadata":{"kind":kind,"byte_length":1,"mime_type":null}}]}}),
                };
                writer.reply(&probe, response).await.unwrap();
                let target = if matches!(kind, "directory" | "unavailable") { "/workspace/main.rs" } else { "/workspace/nested/main.rs" };
                let resolve = reader.read_request().await.unwrap().unwrap();
                assert_eq!(resolve["method"], "host/file/resolve");
                assert_eq!(resolve["params"]["path"], target);
                writer.reply(&resolve, json!({"result":{"name":"main.rs","path":target,"directory":kind == "directory","size":1}})).await.unwrap();
                assert_eq!(result.await.unwrap(), Outcome::FileReferenceResolved { path: target.into(), directory: kind == "directory" });
            }
            store.close().await.unwrap();
        }).await.unwrap();
    }
}
