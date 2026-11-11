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

    pub(super) async fn read_task_activity(
        &self,
        require_complete: bool,
    ) -> Result<agent_protocol::live_activity::TaskActivityState, Failure> {
        self.inner.router.check_read_state()?;
        let revision = self.inner.router.task_activity_revision();
        let agents: Vec<_> = self
            .inner
            .providers
            .all()
            .into_iter()
            .filter(|(_, agent)| !require_complete || agent.requires_restart_snapshot())
            .collect();
        // An unavailable provider has no Host-owned process to interrupt. A
        // broken provider installation must not prevent updating the app.
        let has_sources = !agents.is_empty()
            || (!require_complete && self.inner.providers.startup_errors().next().is_some());
        let listings =
            futures_util::future::join_all(agents.into_iter().map(|(_, agent)| async move {
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
                .await
            }))
            .await;
        let mut native = Vec::new();
        let mut successful = false;
        let mut complete = true;
        for result in listings {
            if let Ok(Ok(pages)) = result {
                let fresh = pages
                    .iter()
                    .flatten()
                    .all(|summary| summary.thread.list_stale != Some(true));
                successful |= fresh;
                complete &= fresh;
                for summary in pages.into_iter().flatten() {
                    if let Some(thread) = self.inner.conversations.summary(summary.thread)? {
                        native.push(thread);
                    }
                }
            } else {
                complete = false;
            }
        }
        let state = self.inner.router.task_activity(native, revision);
        if !successful && has_sources {
            return Err(Failure::new(
                "task_activity_unavailable",
                "タスクの状態を取得できません。",
            ));
        }
        if require_complete && !complete {
            return Err(Failure::new(
                "task_activity_incomplete",
                "実行中のタスクをすべて確認できません。時間をおいて再試行してください。",
            ));
        }
        if complete
            && self.inner.providers.startup_errors().next().is_none()
            && let Some(apns) = self.inner.router.apns()
        {
            apns.activate(state.display.clone());
        }
        Ok(state)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_rpc::routing::HostSession;
    use agent_protocol::live_activity::{MarkSessionRead, ReadTaskActivity, TaskActivityState};
    use agent_protocol::session::SessionRef;

    async fn next_activity(connection: &mut HostSession) -> TaskActivityState {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let agent_protocol::protocol::Notification::TaskActivity { state } =
                    agent_protocol::protocol::decode(&connection.recv().await.unwrap()).unwrap()
                {
                    return state;
                }
            }
        })
        .await
        .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activity_uses_only_normal_conversations_even_with_saved_subagent_completions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bex-transcripts.sqlite");
        let session = |id: &str| SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
        let root = session("public-root");
        let child = session("child");
        let grandchild = session("grandchild");
        crate::host_rpc::store::ConversationStore::open(&path)
            .save_state(
                "reads",
                &[&root, &child, &grandchild].map(|session| {
                    serde_json::json!({"session":session,"turn_id":"completed","unread":true})
                }),
            )
            .unwrap();
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
    if (p.sourceKinds.includes('subAgentThreadSpawn')) throw new Error('Subagents are outside system activity');
    result = {data:['running','native-root'].map(id => ({id,name:id,cwd:process.env.CODEX_HOME,
      status:{type:id === 'running' ? 'active' : 'idle'}})),nextCursor:null};
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
        let mut conversation = crate::host_rpc::conversations::Conversation::new(&Thread {
            id: Some(session("native-root")),
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
        conversation.id = root.clone();
        service.inner.conversations.save(conversation).unwrap();
        service.start();

        let bootstrap = service.read_task_activity(false).await.unwrap();
        assert!(bootstrap.unread.is_empty());
        assert_eq!(bootstrap.display.current.label, "実行中 1件");
        let listed = service
            .list_sessions(&op::ListSessions::new(Default::default()))
            .await
            .unwrap();
        assert!(
            listed
                .data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&root))
        );
        let first = service.read_task_activity(false).await.unwrap();
        assert_eq!(first.unread, [(root, "completed".into())]);
        assert_eq!(first.display.current.label, "実行中 1件 · 完了 1件");
        let repeated = service.read_task_activity(false).await.unwrap();
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
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new(
            [],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let id = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        service.inner.router.task_activity(
            vec![Thread {
                id: Some(id.clone()),
                ..Default::default()
            }],
            0,
        );
        let mut phone = service.open_session();
        let mut desktop = service.open_session();
        for (index, reader) in [(0, phone.id()), (1, desktop.id())] {
            let turn_id: agent_protocol::ids::TurnId = format!("turn-{index}").into();
            service.inner.router.session_change(
                &id,
                agent_protocol::session::SessionChange::Turn {
                    turn: agent_protocol::models::Turn {
                        id: turn_id.clone(),
                        status: agent_protocol::execution::TurnStatus::Completed,
                        ..Default::default()
                    },
                    completed: true,
                },
            );
            for connection in [&mut phone, &mut desktop] {
                assert_eq!(
                    next_activity(connection).await.unread,
                    vec![(id.clone(), turn_id.clone())]
                );
            }
            let reply = service
                .dispatch(
                    reader,
                    &Call::MarkSessionRead(MarkSessionRead {
                        session: id.clone(),
                        turn_id,
                    }),
                )
                .await
                .unwrap();
            assert!(matches!(
                agent_protocol::protocol::decode::<Response<agent_protocol::models::Empty>>(
                    &reply.initial
                )
                .unwrap(),
                Response::Success { .. }
            ));
            for connection in [&mut phone, &mut desktop] {
                assert!(next_activity(connection).await.unread.is_empty());
            }
        }
        // A fresh client and a restarted Host must not restore device-local badges.
        let restarted = HostRpcService::new(
            [],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        let reconnected = restarted.open_session();
        let reply = restarted
            .dispatch(
                reconnected.id(),
                &Call::ReadTaskActivity(ReadTaskActivity {}),
            )
            .await
            .unwrap();
        let Response::Success { result } =
            agent_protocol::protocol::decode::<Response<TaskActivityState>>(&reply.initial)
                .unwrap()
        else {
            panic!("read shared activity")
        };
        assert!(result.unread.is_empty());
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
}
