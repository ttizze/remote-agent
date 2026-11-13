//! Task activity shared with every client, Live Activities and APNs.
use super::*;

impl HostRpcService {
    pub(super) fn register_live_activity(
        &self,
        session: SessionId,
        params: &agent_protocol::live_activity::RegisterLiveActivity,
    ) -> Result<agent_protocol::live_activity::LiveActivityRegistration, Failure> {
        params
            .validate()
            .map_err(|error| Failure::new("invalid_params", error))?;
        let enabled = self.inner.router.apns().is_some();
        if enabled {
            self.inner
                .router
                .register_live_activity(session, params)
                .map_err(|error| Failure::new("live_activity_failed", error))?;
        }
        Ok(agent_protocol::live_activity::LiveActivityRegistration { enabled })
    }

    pub(super) fn unregister_live_activity(
        &self,
        session: SessionId,
        params: &agent_protocol::live_activity::UnregisterLiveActivity,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let principal = self
            .inner
            .router
            .principal(session)
            .map_err(|error| Failure::new("connection_closed", error))?;
        if let Some(apns) = self.inner.router.apns() {
            apns.unregister(&principal, &params.activity_id);
        }
        Ok(agent_protocol::models::Empty {})
    }

    /// Updating must inspect provider-owned processes as well as saved Bex
    /// executions. This admission read never publishes client list state.
    pub(super) async fn update_task_snapshot(
        &self,
    ) -> Result<agent_protocol::live_activity::TaskActivityState, Failure> {
        let agents = self
            .inner
            .providers
            .all()
            .into_iter()
            .filter(|(_, agent)| agent.requires_restart_snapshot())
            .collect::<Vec<_>>();
        let listings = futures_util::future::join_all(agents.into_iter().map(
            |(provider, agent)| async move {
                (
                    provider,
                    tokio::time::timeout(
                        SESSION_LIST_TIMEOUT,
                        session_pages(
                            agent.as_ref(),
                            "",
                            SessionListScope::Loaded,
                            100,
                            100,
                            None,
                            None,
                        )
                        .try_collect::<Vec<_>>(),
                    )
                    .await,
                )
            },
        ))
        .await;
        let mut headers: std::collections::BTreeMap<_, _> = self
            .inner
            .conversation
            .storage
            .headers(true)
            .map_err(super::super::runtime::storage_failure)?
            .into_iter()
            .map(|(id, header, _)| (id, header))
            .collect();
        let mut observed = std::collections::HashSet::new();
        let mut inspected = std::collections::HashSet::new();
        for (provider, result) in listings {
            let pages = result.ok().and_then(Result::ok).ok_or_else(incomplete)?;
            inspected.insert(provider);
            for summary in pages.into_iter().flatten() {
                let native_status = summary.thread.status;
                if summary.thread.list_stale == Some(true) {
                    return Err(incomplete());
                }
                let Some(thread) = self
                    .inner
                    .conversation
                    .storage
                    .native_summary(summary.thread)
                    .map_err(super::super::runtime::storage_failure)?
                else {
                    continue;
                };
                let Some(id) = thread.id.clone() else {
                    continue;
                };
                let header = headers.entry(id.clone()).or_insert_with(|| {
                    super::super::storage::history::header(
                        &agent_protocol::models::ThreadResponse {
                            thread,
                            ..Default::default()
                        },
                        super::super::runtime::now(),
                    )
                });
                // A live source cannot be hidden by another binding's idle list entry.
                let first = observed.insert(id);
                if header.details.activity.latest
                    != Some(agent_protocol::models::TurnStatus::Running)
                    && (first
                        || !agent_protocol::models::task_active(
                            None,
                            header.details.activity.status,
                        ))
                {
                    header.details.activity.status = native_status;
                }
            }
        }
        for (id, header) in &mut headers {
            // A complete loaded-list is evidence that absent sources have no
            // provider-owned work. This is local to restart admission.
            if !observed.contains(id)
                && inspected.contains(&header.state.provider)
                && header.details.activity.latest
                    != Some(agent_protocol::models::TurnStatus::Running)
            {
                header.details.activity.status = agent_protocol::models::SessionStatus::Idle;
            }
        }
        Ok(agent_protocol::conversation::records::list_activity(
            headers.iter(),
        ))
    }

    pub async fn enable_apns(
        &self,
        path: &std::path::Path,
        host_name: &str,
        host_id: &str,
    ) -> anyhow::Result<()> {
        self.inner
            .router
            .set_apns(crate::apns::Apns::load(path, host_name, host_id).await?);
        Ok(())
    }
}

pub(super) fn incomplete() -> Failure {
    Failure::new(
        "task_activity_incomplete",
        "実行中のタスクをすべて確認できません。時間をおいて再試行してください。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_reports_invalid_storage() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("bex-conversations.json"), b"invalid").unwrap();
        let service = HostRpcService::new([], ProjectStore::new(root.path().join("projects.json")));
        assert!(
            service
                .start()
                .unwrap_err()
                .contains("bex-conversations.json")
        );
    }
    use agent_protocol::{
        conversation::records,
        live_activity::{MarkSessionRead, TaskActivityState},
        session::SessionRef,
        sync::Scope,
    };
    struct ListReader {
        updates: crate::host_rpc::routing::HostSubscription,
        replica: records::Replica,
    }
    async fn open_list(service: &HostRpcService, connection: u64) -> ListReader {
        let reply = service
            .dispatch(
                connection,
                &Call::SubscribeList(records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None,
                }),
            )
            .await
            .unwrap();
        let Response::Success { result } =
            agent_protocol::protocol::decode::<Response<records::OpenedList>>(&reply.initial)
                .unwrap()
        else {
            panic!("list subscription failed")
        };
        ListReader {
            updates: reply.updates.unwrap(),
            replica: records::Replica::new(Scope::List, 1)
                .apply(1, result.initial)
                .unwrap(),
        }
    }
    async fn next_activity(reader: &mut ListReader) -> TaskActivityState {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let message = agent_protocol::protocol::decode::<records::Message>(
                &reader.updates.recv().await.unwrap(),
            )
            .unwrap();
            reader.replica = reader.replica.apply(1, message).unwrap();
            records::list_rows_activity(reader.replica.rows().values())
        })
        .await
        .expect("committed list update arrives")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activity_uses_only_normal_conversations_even_with_saved_subagent_completions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let session = |id: &str| SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
        let root = session("public-root");
        std::fs::write(
            directory.path().join("bex-read-state.json"),
            serde_json::to_vec(&["public-root", "child", "grandchild"].map(
                |id| serde_json::json!({"session":session(id),"turn_id":"completed","unread":true}),
            ))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(directory.path().join("bex-conversations.json"), serde_json::to_vec(&[serde_json::json!({"id":root,"active":session("native-root"),"cwd":directory.path(),"name":null,"preview":null,"bindings":[{"session":session("native-root"),"covered":null}],"segments":[],"handoffs":[],"startup":null})]).unwrap()).unwrap();
        let program = directory.path().join("codex-fixture");
        std::fs::write(&program, r##"#!/usr/bin/env node
const rl = require('node:readline').createInterface({input:process.stdin});
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const reads = [];
rl.on('line', line => {
  const m = JSON.parse(line), p = m.params;
  if (m.id === undefined) return;
  let result = {};
  if (m.method === 'initialize') result = {userAgent:'activity-fixture',codexHome:process.env.CODEX_HOME};
  if (m.method === 'fixture/reads') result = reads;
  if (m.method === 'thread/loaded/list') result = {data:['running','child','grandchild'],nextCursor:null};
  if (m.method === 'thread/read') {
    reads.push(p.threadId);
    result = {thread:{id:p.threadId,parentThreadId:{child:'native-root',grandchild:'child'}[p.threadId],
      cwd:process.env.CODEX_HOME,status:{type:'active'}}};
  }
  if (m.method === 'thread/list') {
    result = {data:['native-root'].map(id => ({id,name:id,cwd:process.env.CODEX_HOME,
      status:{type:'idle'}})),nextCursor:null};
  }
  send({id:m.id,result});
});
"##).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = Arc::new(
            codex_app_server::CodexAppServer::spawn(crate::providers::codex::Config {
                program,
                codex_home: Some(directory.path().to_owned()),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let service = HostRpcService::new(
            [crate::providers::codex::Codex::new(Ok(server.clone())).into()],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let storage = &service.inner.conversation.storage;
        storage.queue_catalog(ProviderKind::Codex, 0).unwrap();
        let claim = storage.claim(0, 1).unwrap().remove(0);
        let mut stale = super::super::super::storage::history::header(
            &agent_protocol::models::ThreadResponse {
                thread: Thread {
                    id: Some(session("running")),
                    status: agent_protocol::models::SessionStatus::Running,
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        );
        stale.details.activity.stale = true;
        storage
            .catalog_page(
                &claim,
                &[(session("running"), stale)],
                None,
                i64::MAX - 60_000,
            )
            .unwrap();
        service.start().unwrap();

        let bootstrap = service.update_task_snapshot().await.unwrap();
        assert_eq!(bootstrap.unread, [(root.clone(), "completed".into())]);
        assert_eq!(bootstrap.display.current.label, "実行中 1件 · 完了 1件");
        let listed = service
            .search_list(&agent_protocol::conversation::records::SearchList {
                term: String::new(),
                limit: Some(10_000),
            })
            .await
            .unwrap();
        assert!(
            listed
                .data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&root))
        );
        let first = service.update_task_snapshot().await.unwrap();
        assert_eq!(first.unread, [(root, "completed".into())]);
        assert_eq!(first.display.current.label, "実行中 1件 · 完了 1件");
        let repeated = service.update_task_snapshot().await.unwrap();
        assert_eq!(repeated, first);
        let reads = server
            .request::<_, Vec<String>>("fixture/reads", &serde_json::json!({}))
            .await
            .unwrap()
            .outcome
            .unwrap();
        assert!(
            !reads.iter().any(|id| id == "native-root"),
            "saved completions need no extra native reads"
        );
        drop(service);
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reading_on_either_device_broadcasts_to_both_and_reconnect_reads_the_shared_state() {
        use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
        use agent_protocol::conversation::{Command, InputContent, records};
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
        let service = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let phone = service.open_session();
        let desktop = service.open_session();
        let mut phone_list = open_list(&service, phone.id()).await;
        let mut desktop_list = open_list(&service, desktop.id()).await;
        let id = session_uuid();
        let _: records::Opened = call(
            &service,
            phone.id(),
            Call::CreateConversation(records::Create {
                workspace: None,
                provider: agent_protocol::session::ProviderKind::Codex,
                session: id.clone(),
                subscription: uuid::Uuid::new_v4(),
                cwd: Some(directory.path().to_string_lossy().into_owned()),
                selection: Default::default(),
                initial: None,
            }),
        )
        .await
        .unwrap();
        for reader in [phone.id(), desktop.id()] {
            let input = uuid::Uuid::new_v4();
            let _: records::Acknowledged = call(
                &service,
                reader,
                Call::ChangeConversation(records::Change {
                    session: id.clone(),
                    operation: input,
                    command: Command::Submit {
                        id: input.to_string().into(),
                        content: InputContent {
                            parts: vec![op::Input::Text {
                                text: "complete".into(),
                            }],
                            model: None,
                            effort: None,
                            service_tier: None,
                            follow_up: Default::default(),
                        },
                    },
                }),
            )
            .await
            .unwrap();
            let mut completed = None;
            for connection in [&mut phone_list, &mut desktop_list] {
                loop {
                    let update = next_activity(connection).await;
                    if let Some((_, turn)) =
                        update.unread.iter().find(|(session, _)| session == &id)
                    {
                        completed = Some(turn.clone());
                        break;
                    }
                }
            }
            let _: agent_protocol::models::Empty = call(
                &service,
                reader,
                Call::MarkSessionRead(MarkSessionRead {
                    session: id.clone(),
                    turn_id: completed.unwrap(),
                }),
            )
            .await
            .unwrap();
            for connection in [&mut phone_list, &mut desktop_list] {
                loop {
                    if next_activity(connection).await.unread.is_empty() {
                        break;
                    }
                }
            }
        }
        let restarted = HostRpcService::new(
            [],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let reconnected = restarted.open_session();
        let list = open_list(&restarted, reconnected.id()).await;
        let state = restarted.inner.conversation.storage.activity().unwrap();
        let records::Message::Snapshot { cursor, .. } =
            restarted.inner.conversation.storage.list(None).unwrap()
        else {
            panic!("initial list")
        };
        assert_eq!(list.replica.cursor(), Some(cursor));
        assert!(state.unread.is_empty());
    }
    fn session_uuid() -> SessionRef {
        SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap()
    }

    #[tokio::test]
    async fn live_activity_registration_accepts_both_environments_and_rejects_malformed_tokens() {
        use super::*;
        use agent_protocol::live_activity::{
            LiveActivityRegistration, PushEnvironment, RegisterLiveActivity,
        };
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [crate::providers::codex::Codex::new(Err("provider is offline".into())).into()],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let connection = service.open_session();
        let mut params = RegisterLiveActivity {
            activity_id: Some("activity".into()),
            allow_start: false,
            token: vec![1; 32],
            environment: PushEnvironment::Sandbox,
        };
        // One Host serves store and directly installed development builds of the same app.
        for (configured, environment) in [
            (false, PushEnvironment::Sandbox),
            (true, PushEnvironment::Sandbox),
            (true, PushEnvironment::Production),
        ] {
            if configured {
                service
                    .inner
                    .router
                    .set_apns(Some(crate::apns::Apns::testing()));
            }
            params.environment = environment;
            let response = service
                .dispatch(connection.id(), &Call::RegisterLiveActivity(params.clone()))
                .await
                .unwrap();
            let Response::Success { result } = agent_protocol::protocol::decode::<
                Response<LiveActivityRegistration>,
            >(&response.initial)
            .unwrap() else {
                panic!("unconfigured APNs must keep local updates available");
            };
            assert_eq!(result.enabled, configured);
        }
        params.token.clear();
        let response = service
            .dispatch(connection.id(), &Call::RegisterLiveActivity(params))
            .await
            .unwrap();
        assert!(matches!(
            agent_protocol::protocol::decode::<Response<LiveActivityRegistration>>(
                &response.initial
            )
            .unwrap(),
            Response::Failure { .. }
        ));
    }
    #[tokio::test]
    async fn live_activity_tracks_committed_work_after_disconnect_and_revocation_prevents_reregistration()
     {
        use crate::host_rpc::runtime::native_fixture::call;
        use agent_protocol::live_activity::{
            LiveActivityRegistration, PushEnvironment, RegisterLiveActivity,
        };
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let apns = crate::apns::Apns::testing();
        service.inner.router.set_apns(Some(apns.clone()));
        let id = SessionRef::new(ProviderKind::Codex, "running-native".into()).unwrap();
        service
            .inner
            .conversation
            .storage
            .import(
                &id,
                &agent_protocol::models::ThreadResponse {
                    thread: Thread {
                        id: Some(id.clone()),
                        status: agent_protocol::models::SessionStatus::Running,
                        turns: Some(Vec::new()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Some(&id),
                super::super::super::runtime::now(),
            )
            .unwrap();
        apns.activate(
            service
                .inner
                .conversation
                .storage
                .activity()
                .unwrap()
                .display,
        );
        let phone = service.open_authenticated_session("phone".into());
        let params = RegisterLiveActivity {
            activity_id: Some("activity".into()),
            allow_start: false,
            token: vec![1; 32],
            environment: PushEnvironment::Sandbox,
        };
        let registration: LiveActivityRegistration = call(
            &service,
            phone.id(),
            Call::RegisterLiveActivity(params.clone()),
        )
        .await
        .unwrap();
        assert!(registration.enabled);
        let payload = apns.test_payloads(crate::apns::now()).pop().unwrap();
        assert_eq!(
            payload["aps"]["content-state"]["display"]["current"]["label"],
            "実行中 1件"
        );
        service.close_session(phone.id());
        assert_eq!(apns.test_payloads(crate::apns::now()).len(), 1);
        super::super::super::runtime::Runtime::event(
            &service,
            &id,
            agent_protocol::conversation::Event::NativeStatus {
                provider: ProviderKind::Codex,
                source: id.id.clone(),
                status: agent_protocol::models::SessionStatus::Idle,
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if apns
                    .test_payloads(crate::apns::now())
                    .iter()
                    .any(|payload| payload["aps"]["event"] == "end")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("committed completion reaches APNs without a connected phone");
        let reconnected = service.open_authenticated_session("phone".into());
        service.revoke_device("phone");
        assert!(apns.test_payloads(crate::apns::now()).is_empty());
        assert!(
            service
                .register_live_activity(reconnected.id(), &params)
                .is_err()
        );
        assert!(apns.test_payloads(crate::apns::now()).is_empty());
    }
}
