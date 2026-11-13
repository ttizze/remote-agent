use super::*;
use crate::presentation::files::{FileTreeAction, FileTreeKey, tree::TreeWork};
use agent_protocol::file_tree::{ListTreeEntries, SearchTreeEntries, TreeEntries};

pub(crate) fn file_tree_action(
    snapshot: &mut Snapshot,
    key: FileTreeKey,
    action: FileTreeAction,
) -> Vec<Effect> {
    if (ListTreeEntries {
        root: key.root.clone(),
        directory: String::new(),
    })
    .validate()
    .is_err()
    {
        return Vec::new();
    }
    let connection = snapshot.connection.connection_generation;
    let trees = &mut Arc::make_mut(&mut snapshot.files.workspace).file_trees;
    let work = trees.action(key, action, connection);
    if snapshot.connection.connected {
        effects(work)
    } else {
        trees.disconnected();
        Vec::new()
    }
}
pub(crate) fn reconnect_file_trees(snapshot: &mut Snapshot) -> Vec<Effect> {
    effects(
        Arc::make_mut(&mut snapshot.files.workspace)
            .file_trees
            .reconnect(snapshot.connection.connection_generation),
    )
}
pub(crate) fn refresh_file_trees(snapshot: &mut Snapshot, path: &str) -> Vec<Effect> {
    if !snapshot.connection.connected {
        return Vec::new();
    }
    effects(
        Arc::make_mut(&mut snapshot.files.workspace)
            .file_trees
            .refresh_matching(path, snapshot.connection.connection_generation),
    )
}

fn effects(work: Vec<TreeWork>) -> Vec<Effect> {
    work.into_iter()
        .map(|work| match work {
            TreeWork::Directory {
                key,
                path,
                generation,
            } => Effect::execute(ReadTreeDirectory {
                key,
                path,
                generation,
            }),
            TreeWork::Search {
                key,
                query,
                generation,
            } => Effect::execute(DebounceSearchTree(SearchTree {
                key,
                query,
                generation,
            })),
        })
        .collect()
}

#[derive(Debug)]
struct ReadTreeDirectory {
    key: FileTreeKey,
    path: String,
    generation: u64,
}
impl Operation for ReadTreeDirectory {
    no_input!();
    type Output = Result<TreeEntries, String>;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        Ok(context
            .call(&ListTreeEntries {
                root: self.key.root.clone(),
                directory: self.path.clone(),
            })
            .await
            .map_err(|error| error.to_string()))
    }
    fn rejected_output(&self, _: ()) -> Option<Self::Output> {
        Some(Err("フォルダを読み込めませんでした。".into()))
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, result: Self::Output) -> Vec<Effect> {
        effects(
            Arc::make_mut(&mut snapshot.files.workspace)
                .file_trees
                .directory(&self.key, self.path, self.generation, result),
        )
    }
}

#[derive(Debug)]
struct DebounceSearchTree(SearchTree);
impl Operation for DebounceSearchTree {
    no_input!();
    type Output = Result<(), String>;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(&self, _: (), _: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Ok(Ok(()))
    }
    fn rejected_output(&self, _: ()) -> Option<Self::Output> {
        Some(Err("ファイルを検索できませんでした。".into()))
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, result: Self::Output) -> Vec<Effect> {
        if let Err(error) = result {
            return self.0.apply(snapshot, pane_id, Err(error));
        }
        // The timer and the RPC have separate lifetimes. New input must start
        // its own debounce while a prior RPC is running, and stale timers must
        // never issue an obsolete Host search.
        if snapshot
            .files
            .workspace
            .file_trees
            .search_current(&self.0.key, self.0.generation)
        {
            vec![Effect::continuation(self.0)]
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug)]
struct SearchTree {
    key: FileTreeKey,
    query: String,
    generation: u64,
}
impl Operation for SearchTree {
    no_input!();
    type Output = Result<TreeEntries, String>;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        Ok(context
            .call(&SearchTreeEntries {
                root: self.key.root.clone(),
                query: self.query.clone(),
                limit: 200,
                files_only: false,
            })
            .await
            .map_err(|error| error.to_string()))
    }
    fn rejected_output(&self, _: ()) -> Option<Self::Output> {
        Some(Err("ファイルを検索できませんでした。".into()))
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, result: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.files.workspace)
            .file_trees
            .searched(&self.key, self.generation, result);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::test_host as host_fixture;

    use super::*;
    use crate::presentation::files::FileTreeSurface;
    use serde_json::json;
    fn key(root: &str) -> FileTreeKey {
        FileTreeKey {
            owner: "browser".into(),
            root: root.into(),
        }
    }

    #[test]
    fn only_the_latest_debounce_can_start_a_host_search() {
        let mut snapshot = Snapshot::default();
        let trees = &mut Arc::make_mut(&mut snapshot.files.workspace).file_trees;
        trees.action(
            key("/workspace"),
            FileTreeAction::Open {
                surface: FileTreeSurface::Mobile,
                locale: "en-US".into(),
                workspace_root: "/workspace".into(),
            },
            1,
        );
        let mut query = |text: &str| {
            trees
                .action(
                    key("/workspace"),
                    FileTreeAction::Query { text: text.into() },
                    1,
                )
                .into_iter()
                .find_map(|work| {
                    let TreeWork::Search {
                        key,
                        query,
                        generation,
                    } = work
                    else {
                        return None;
                    };
                    Some(DebounceSearchTree(SearchTree {
                        key,
                        query,
                        generation,
                    }))
                })
                .unwrap()
        };
        let old = query("old");
        let current = query("current");
        assert!(old.apply(&mut snapshot, "", Ok(())).is_empty());
        assert_eq!(current.apply(&mut snapshot, "", Ok(())).len(), 1);
        let trees = &mut Arc::make_mut(&mut snapshot.files.workspace).file_trees;
        let pending = trees.action(
            key("/workspace"),
            FileTreeAction::Query {
                text: "pending".into(),
            },
            1,
        );
        let TreeWork::Search {
            key: pending_key,
            query,
            generation,
        } = pending.into_iter().next().unwrap()
        else {
            panic!()
        };
        trees.action(
            key("/workspace"),
            FileTreeAction::Query {
                text: String::new(),
            },
            1,
        );
        assert!(
            DebounceSearchTree(SearchTree {
                key: pending_key,
                query,
                generation
            })
            .apply(&mut snapshot, "", Ok(()))
            .is_empty()
        );
    }

    #[test]
    fn reducer_owns_tree_updates_without_mutating_published_snapshots() {
        let mut before = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::make_mut(&mut Arc::make_mut(before.panes.primary_mut()).navigation).cwd =
            "/another-thread".into();
        let (mut next, effects) = crate::state::reduce(
            &before,
            "",
            crate::state::Event::Intent(Intent::FileTree {
                key: key("/workspace"),
                action: FileTreeAction::Open {
                    surface: FileTreeSurface::Mobile,
                    locale: "en-US".into(),
                    workspace_root: "/workspace".into(),
                },
            }),
        );
        assert_eq!(effects.len(), 1);
        assert!(!before.files.file_tree(key("/workspace")).pending);
        assert!(next.files.file_tree(key("/workspace")).pending);
        assert!(!next.files.file_tree(key("/workspace")).ready);
        assert_eq!(
            file_tree_action(
                &mut next,
                key("/workspace"),
                FileTreeAction::Query {
                    text: "main".into()
                }
            )
            .len(),
            1,
            "the tree searches its owning workspace even when a side chat changed Store navigation"
        );
        // Invalid roots never reach a Host operation or create client state.
        assert!(file_tree_action(&mut next, key("relative"), FileTreeAction::Refresh).is_empty());
    }

    #[tokio::test]
    async fn disconnected_directory_failure_settles_in_the_tree_and_can_be_retried() {
        let store = crate::store::Store::offline(Snapshot::default(), None);
        let open = Intent::FileTree {
            key: key("/workspace"),
            action: FileTreeAction::Open {
                surface: FileTreeSurface::Mobile,
                locale: "en-US".into(),
                workspace_root: "/workspace".into(),
            },
        };
        let _ = store.dispatch(open.clone()).await;
        let mut updates = store.subscribe();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            updates.wait_for(|snapshot| {
                let tree = snapshot.files.file_tree(key("/workspace"));
                !tree.pending && tree.error.is_some()
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            store
                .snapshot()
                .error_notices("", crate::presentation::error::ErrorScope::Workspace)
                .is_empty()
        );
        let _ = store.dispatch(open).await;
        updates
            .wait_for(|snapshot| !snapshot.files.file_tree(key("/workspace")).pending)
            .await
            .unwrap();
        assert!(
            store
                .snapshot()
                .files
                .file_tree(key("/workspace"))
                .error
                .is_some()
        );
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_retries_mounted_offline_tree_and_requested_editor_catalog_without_ui_actions()
     {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut initial = Snapshot::default();
            super::super::ReadFileOpenCapabilities {}.prepare(&mut initial, "").unwrap();
            initial.files.preferred_editor = Some(agent_protocol::editors::EditorId::Cursor);
            super::super::ReadFileOpenCapabilities {}.apply(
            &mut initial,
            "",
                agent_protocol::editors::FileOpenCapabilities { editors: vec![agent_protocol::editors::EditorId::Cursor], reveal: None },
            );
            let (mut initial, _) = crate::state::reduce(&initial, "", Event::Disconnected);
            assert!(initial.files.file_open_capabilities.is_none());
            let key = FileTreeKey { owner: "mobile".into(), root: "/workspace".into() };
            assert!(file_tree_action(&mut initial, key.clone(), FileTreeAction::Open {
                surface: crate::presentation::files::FileTreeSurface::Mobile,
                locale: "en-US".into(), workspace_root: "/workspace".into(),
            }).is_empty());
            assert!(initial.files.file_tree(key.clone()).error.is_some());
            let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
            let store = crate::store::Store::new(peer, initial, None);
            let mut methods = Vec::new();
            for _ in 0..5 {
                let request = reader.read_request().await.unwrap().unwrap();
                let method = request["method"].as_str().unwrap();
                let reply = match method {
                    "host/file/openCapabilities" => json!({"editors":["zed"],"reveal":null}),
                    "host/file/tree" => json!({"entries":[{"path":"new.rs","kind":"file","ignored":false}],"truncated":false}),
                    "host/list/subscribe" => json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5}),
                    "host/model/list" => json!({"data":[],"nextCursor":null}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
                    other => panic!("unexpected reconnect request: {other}"),
                };
                methods.push(method.to_owned());
                writer.reply(&request, json!({"result":reply})).await.unwrap();
            }
            let mut updates = store.subscribe();
            updates.wait_for(|snapshot| snapshot.files.file_tree(key.clone()).ready && snapshot.files.file_open_capabilities.is_some()).await.unwrap();
            let snapshot = store.snapshot();
            assert_eq!(snapshot.files.file_tree(key.clone()).rows[0].path, "new.rs");
            assert!(snapshot.files.file_tree(key).error.is_none());
            assert_eq!(snapshot.files.preferred_editor, Some(agent_protocol::editors::EditorId::Cursor));
            assert_eq!(snapshot.files.file_open_capabilities.as_ref().unwrap().editors, [agent_protocol::editors::EditorId::Zed]);
            assert_eq!(methods.iter().filter(|method| *method == "host/file/openCapabilities").count(), 1);
            store.close().await.unwrap();
        }).await.unwrap();
    }
}
