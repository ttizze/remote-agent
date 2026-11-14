use super::*;

#[test]
fn recovery_requires_a_terminal_capacity_code_without_a_provider_retry() {
    assert!(capacity_failure(
        TurnStatus::Failed,
        Some("serverOverloaded"),
        false
    ));
    for status in [
        TurnStatus::Running,
        TurnStatus::Completed,
        TurnStatus::Interrupted,
        TurnStatus::Unknown,
    ] {
        assert!(!capacity_failure(status, Some("serverOverloaded"), false));
    }
    for code in [
        None,
        Some("503"),
        Some("unauthorized"),
        Some("usageLimitExceeded"),
        Some("httpConnectionFailed"),
    ] {
        assert!(!capacity_failure(TurnStatus::Failed, code, false));
    }
    assert!(!capacity_failure(
        TurnStatus::Failed,
        Some("serverOverloaded"),
        true
    ));
}

#[cfg(unix)]
struct Fixture {
    directory: tempfile::TempDir,
    server: Arc<codex_app_server::CodexAppServer>,
    service: HostRpcService,
    session: SessionRef,
    _lease: crate::host_rpc::routing::SessionLease,
}

#[cfg(unix)]
impl Fixture {
    async fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex-fixture");
        std::fs::write(&program, r##"#!/usr/bin/env node
const rl = require('node:readline').createInterface({input:process.stdin});
const starts = [], interrupts = [], config = {};
let current = null, status = 'idle', held;
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const reply = (m, result) => send({id:m.id, result});
function event(method, params) {
  if (method === 'turn/started') { current = params.turn; status = 'active'; }
  if (method === 'turn/completed') { current = params.turn; status = 'idle'; }
  send({method, params});
}
function start(m) {
  const turn = {id:`turn-${starts.length}`,status:'inProgress',items:[]};
  event('turn/started', {threadId:'task', turn});
  if (config.completeImmediately) event('turn/completed', {threadId:'task',turn:{...turn,status:'completed'}});
  reply(m, {turn});
}
rl.on('line', line => {
  const m = JSON.parse(line), p = m.params;
  if (m.id === undefined) return;
  switch (m.method) {
    case 'initialize': return reply(m,{userAgent:'capacity-fixture',codexHome:process.env.CODEX_HOME});
    case 'fixture/event': event(p.method, p.params); return reply(m,{});
    case 'fixture/config': Object.assign(config,p); return reply(m,{});
    case 'fixture/log': return reply(m,{starts,interrupts});
    case 'fixture/release': start(held); held = undefined; return reply(m,{});
    case 'thread/read': return reply(m,{thread:{id:'task',cwd:process.env.CODEX_HOME,
      status:{type:config.status ?? status},parentThreadId:config.parent ?? null,turns:current ? [current] : []},model:'chosen-model'});
    case 'turn/start':
      starts.push(p);
      if (config.rejectStart) return send({id:m.id,error:{code:-32000,message:'start refused',codexErrorInfo:'unauthorized'}});
      if (config.holdStart) { held = m; return; }
      return start(m);
    case 'turn/interrupt':
      interrupts.push(p.turnId);
      event('turn/completed',{threadId:'task',turn:{id:p.turnId,status:'interrupted',items:[]}});
      return reply(m,{});
    default: return reply(m,{});
  }
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
        service.start().unwrap();
        let session = SessionRef::new(ProviderKind::Codex, "task".into()).unwrap();
        let lease = service
            .inner
            .router
            .retain_execution(session.clone())
            .unwrap();
        let fixture = Self {
            directory,
            server,
            service,
            session,
            _lease: lease,
        };
        fixture.submit("original request").await;
        fixture
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.server
            .request::<_, serde_json::Value>(method, &params)
            .await
            .unwrap()
            .outcome
            .unwrap()
    }

    async fn event(&self, method: &str, params: serde_json::Value) {
        self.request(
            "fixture/event",
            serde_json::json!({"method":method,"params":params}),
        )
        .await;
        // The adapter's response barrier proves all earlier native notifications
        // have been applied by the real Host event pump.
        self.settle().await;
    }

    async fn settle(&self) {
        self.service
            .inner
            .providers
            .get(ProviderKind::Codex)
            .unwrap()
            .state("task")
            .await
            .unwrap();
    }

    async fn fail(&self, turn: &str, code: &str) {
        self.event(
            "turn/completed",
            serde_json::json!({"threadId":"task","turn":{
                "id":turn,"status":"failed","items":[],"error":{
                    "message":"Selected model is at capacity. Please try a different model.",
                    "codexErrorInfo":code
                }
            }}),
        )
        .await;
        self.event(
            "thread/status/changed",
            serde_json::json!({"threadId":"task","status":{"type":"idle"}}),
        )
        .await;
    }

    async fn submit(&self, text: &str) {
        let _serial = self
            .service
            .inner
            .router
            .submission_lock(&self.session)
            .lock_owned()
            .await;
        self.service
            .submit_input(
                &self.session,
                &op::Submission {
                    fresh_context: false,
                    thread_id: self.session.clone(),
                    client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    input: vec![op::Input::Text { text: text.into() }],
                    model: Some(agent_protocol::models::ModelRef {
                        provider: ProviderKind::Codex,
                        id: "chosen-model".into(),
                    }),
                    effort: Some("high".into()),
                    service_tier: Some("fast".into()),
                    follow_up: Default::default(),
                },
            )
            .await
            .unwrap();
    }

    fn turn(&self, id: &str) -> Arc<Turn> {
        self.service
            .inner
            .router
            .current_turn(&self.session, id)
            .unwrap()
    }

    async fn advance(&self, seconds: u64) {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(seconds)).await;
        tokio::time::resume();
    }

    async fn starts(&self, expected: usize) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let log = self.request("fixture/log", serde_json::json!({})).await;
                if log["starts"].as_array().unwrap().len() == expected {
                    return log;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    async fn stop(&self, id: &str) {
        self.service
            .interrupt(
                &self.session,
                &op::Interrupt {
                    thread_id: self.session.clone(),
                    turn_id: id.into(),
                },
            )
            .await
            .unwrap();
    }

    async fn shutdown(self) {
        self.service.shutdown_owned_processes().await;
        drop(self.directory);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_capacity_failures_continue_the_same_thread_five_times_without_replaying_input() {
    let fixture = Fixture::new().await;
    for (index, delay) in [2, 4, 8, 16, 30].into_iter().enumerate() {
        let id = format!("turn-{}", index + 1);
        fixture.fail(&id, "serverOverloaded").await;
        assert_eq!(fixture.turn(&id).status, TurnStatus::Running);
        // Each continuation is numbered of the Host's budget.
        assert_eq!(
            fixture.turn(&id).error.as_ref().unwrap().retry,
            Some(agent_protocol::execution::RetryEvidence::new(
                Some(index as u32 + 1),
                Some(5)
            ))
        );
        // Duplicate completion must neither spend another attempt nor send twice.
        fixture.fail(&id, "serverOverloaded").await;
        fixture.advance(delay - 1).await;
        assert_eq!(
            fixture.request("fixture/log", serde_json::json!({})).await["starts"]
                .as_array()
                .unwrap()
                .len(),
            index + 1
        );
        fixture.advance(1).await;
        let log = fixture.starts(index + 2).await;
        assert_eq!(
            log["starts"][index + 1],
            serde_json::json!({"threadId":"task","input":[],"model":"chosen-model","effort":"high","serviceTierForTurn":"fast","summary":"detailed"})
        );
        fixture
            .service
            .inner
            .providers
            .get(ProviderKind::Codex)
            .unwrap()
            .state("task")
            .await
            .unwrap();
        assert_eq!(fixture.turn(&id).status, TurnStatus::Failed);
        assert!(fixture.turn(&id).error.is_none());
    }
    fixture.fail("turn-6", "serverOverloaded").await;
    assert_eq!(fixture.turn("turn-6").status, TurnStatus::Failed);
    assert!(
        fixture
            .turn("turn-6")
            .error
            .as_ref()
            .unwrap()
            .retry
            .is_none()
    );
    fixture.advance(60).await;
    fixture.starts(6).await;
    assert!(fixture.service.inner.capacity.lock().unwrap().is_empty());
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stop_cancels_the_wait_and_stop_before_terminal_failure_prevents_recovery() {
    for before_failure in [false, true] {
        let fixture = Fixture::new().await;
        if before_failure {
            fixture.stop("turn-1").await;
        }
        fixture.fail("turn-1", "serverOverloaded").await;
        if !before_failure {
            fixture.stop("turn-1").await;
            assert_eq!(fixture.turn("turn-1").status, TurnStatus::Interrupted);
        }
        fixture.advance(60).await;
        fixture.starts(1).await;
        assert!(fixture.service.inner.capacity.lock().unwrap().is_empty());
        fixture.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn new_input_overtakes_a_waiting_retry_and_gets_a_fresh_budget() {
    let fixture = Fixture::new().await;
    fixture.fail("turn-1", "serverOverloaded").await;
    fixture.submit("new request").await;
    fixture.advance(2).await;
    let log = fixture.starts(2).await;
    assert_eq!(log["starts"][1]["input"][0]["text"], "new request");
    // Codex summarizes its reasoning only when a turn asks.
    assert_eq!(log["starts"][1]["summary"], "detailed");
    fixture.fail("turn-2", "serverOverloaded").await;
    fixture.advance(2).await;
    fixture.starts(3).await;
    fixture.settle().await;
    fixture.stop("turn-2").await;
    let log = fixture.request("fixture/log", serde_json::json!({})).await;
    assert_eq!(log["interrupts"], serde_json::json!(["turn-3"]));
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_while_the_retry_is_starting_interrupts_the_acknowledged_native_turn() {
    let fixture = Fixture::new().await;
    fixture
        .request("fixture/config", serde_json::json!({"holdStart":true}))
        .await;
    fixture.fail("turn-1", "serverOverloaded").await;
    fixture.advance(2).await;
    fixture.starts(2).await;
    fixture.stop("turn-1").await;
    fixture
        .request("fixture/release", serde_json::json!({}))
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let log = fixture.request("fixture/log", serde_json::json!({})).await;
            if log["interrupts"] == serde_json::json!(["turn-2"]) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.advance(60).await;
    fixture.starts(2).await;
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_continuation_and_completed_continuation_do_not_resend_or_interrupt() {
    for config in [
        serde_json::json!({"rejectStart":true}),
        serde_json::json!({"completeImmediately":true}),
    ] {
        let fixture = Fixture::new().await;
        fixture.request("fixture/config", config).await;
        fixture.fail("turn-1", "serverOverloaded").await;
        fixture.advance(2).await;
        fixture.starts(2).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.service.inner.capacity.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.advance(60).await;
        let log = fixture.starts(2).await;
        assert_eq!(log["interrupts"], serde_json::json!([]));
        fixture.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn typed_error_notifications_supply_missing_terminal_details_without_layering_native_retries()
{
    let fixture = Fixture::new().await;
    for retrying in [true, false] {
        fixture.event("error", serde_json::json!({"threadId":"task","turnId":"turn-1",
            "error":{"message":"capacity","codexErrorInfo":"serverOverloaded"},"willRetry":retrying
        })).await;
        fixture.advance(60).await;
        fixture.starts(1).await;
    }
    fixture
        .event(
            "turn/completed",
            serde_json::json!({"threadId":"task",
                "turn":{"id":"turn-1","status":"failed","items":[]}
            }),
        )
        .await;
    fixture.advance(2).await;
    fixture.starts(2).await;
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn native_recovery_clears_its_notice_without_a_host_continuation() {
    let fixture = Fixture::new().await;
    fixture
        .event(
            "error",
            serde_json::json!({"threadId":"task","turnId":"turn-1",
                "error":{"message":"capacity","codexErrorInfo":"serverOverloaded"},"willRetry":true
            }),
        )
        .await;
    fixture
        .event(
            "turn/completed",
            serde_json::json!({"threadId":"task",
                "turn":{"id":"turn-1","status":"completed","items":[]}
            }),
        )
        .await;
    assert_eq!(fixture.turn("turn-1").status, TurnStatus::Completed);
    assert!(fixture.turn("turn-1").error.is_none());
    fixture.advance(60).await;
    fixture.starts(1).await;
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_compaction_does_not_resume_the_preceding_task() {
    let fixture = Fixture::new().await;
    fixture
        .event(
            "turn/completed",
            serde_json::json!({"threadId":"task",
                "turn":{"id":"turn-1","status":"completed","items":[]}
            }),
        )
        .await;
    fixture.submit("/compact").await;
    fixture.fail("turn-2", "serverOverloaded").await;
    fixture.advance(60).await;
    fixture.starts(2).await;
    assert_eq!(fixture.turn("turn-2").status, TurnStatus::Failed);
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_waiting_question_or_disabled_provider_prevents_native_continuation() {
    use agent_protocol::requests::{Request, RequestBody, RequestTarget};
    for disable in [false, true] {
        let fixture = Fixture::new().await;
        fixture.fail("turn-1", "serverOverloaded").await;
        if disable {
            fixture
                .service
                .inner
                .provider_settings
                .set_enabled(ProviderKind::Codex, false)
                .unwrap();
        } else {
            fixture.service.inner.router.session_change(
                &fixture.session,
                SessionChange::Request {
                    request: Request {
                        id: "question".into(),
                        target: RequestTarget::Session,
                        delivery: agent_protocol::session::RequestDelivery::Awaiting,
                        body: RequestBody::Question { questions: vec![] },
                    },
                },
            );
        }
        fixture.advance(2).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.service.inner.capacity.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.starts(1).await;
        assert_eq!(fixture.turn("turn-1").status, TurnStatus::Failed);
        fixture.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_uses_the_native_binding_after_a_provider_switch() {
    use crate::host_rpc::conversations::{Binding, Conversation};
    let mut fixture = Fixture::new().await;
    fixture
        .event(
            "turn/completed",
            serde_json::json!({"threadId":"task",
                "turn":{"id":"turn-1","status":"completed","items":[]}
            }),
        )
        .await;
    let first = SessionRef::new(ProviderKind::Claude, "previous-provider".into()).unwrap();
    let mut row = Conversation::new(&Thread {
        id: Some(first.clone()),
        cwd: Some(fixture.directory.path().to_str().unwrap().into()),
        ..Default::default()
    })
    .unwrap();
    row.active = fixture.session.clone();
    row.bindings.push(Binding {
        context_start: 0,
        session: fixture.session.clone(),
        covered: None,
    });
    fixture
        .service
        .inner
        .conversations
        .save(row.clone())
        .unwrap();
    fixture.session = first;
    let _lease = fixture
        .service
        .inner
        .router
        .retain_execution(fixture.session.clone())
        .unwrap();
    // A fresh turn is routed through the saved binding and gets qualified IDs.
    fixture.submit("request after switch").await;
    fixture.fail("turn-2", "serverOverloaded").await;
    let qualified = row.identity(&row.active, "turn-2");
    assert_eq!(fixture.turn(&qualified).status, TurnStatus::Running);
    fixture.advance(2).await;
    fixture.starts(3).await;
    fixture.settle().await;
    fixture.stop(&qualified).await;
    let log = fixture.request("fixture/log", serde_json::json!({})).await;
    assert_eq!(log["starts"][2]["threadId"], "task");
    assert_eq!(log["interrupts"], serde_json::json!(["turn-3"]));
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn only_explicit_capacity_failures_on_inactive_root_threads_are_retried() {
    for code in [
        "unauthorized",
        "usageLimitExceeded",
        "httpConnectionFailed",
        "badRequest",
    ] {
        let fixture = Fixture::new().await;
        fixture.fail("turn-1", code).await;
        fixture.advance(60).await;
        fixture.starts(1).await;
        assert_eq!(fixture.turn("turn-1").status, TurnStatus::Failed);
        fixture.shutdown().await;
    }
    for config in [
        serde_json::json!({"parent":"parent"}),
        serde_json::json!({"status":"active"}),
        serde_json::json!({"status":"notLoaded"}),
    ] {
        let fixture = Fixture::new().await;
        fixture.request("fixture/config", config).await;
        fixture.fail("turn-1", "serverOverloaded").await;
        fixture.advance(2).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.service.inner.capacity.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.starts(1).await;
        assert_eq!(fixture.turn("turn-1").status, TurnStatus::Failed);
        fixture.shutdown().await;
    }
}
