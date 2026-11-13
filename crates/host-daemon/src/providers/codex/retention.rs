//! The Host is app-server's only client, so its subscription alone keeps a
//! thread loaded with its MCP servers. App-server unloads an unsubscribed
//! inactive thread by itself; the next start resumes it.
use crate::providers::{AgentChange, Failure};
use agent_protocol::{
    execution::ItemStatus,
    models::SessionStatus,
    session::{ProviderKind, SessionChange},
};
use codex_app_server::CodexAppServer;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Release a subscription after this much silence.
const IDLE: Duration = Duration::from_secs(30 * 60);
/// Running background work keeps its thread while it reports within this time.
const MAX_PIN: Duration = Duration::from_secs(4 * 60 * 60);
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

pub(super) struct Subscriptions {
    activity: Mutex<Activity>,
    /// Held across subscription requests so a release cannot overtake a resume.
    released: tokio::sync::Mutex<HashSet<String>>,
    scopes: Arc<crate::providers::scopes::Scopes>,
    processed: tokio::sync::watch::Receiver<u64>,
    stopped: CancellationToken,
}
impl Subscriptions {
    pub(super) fn new(
        scopes: Arc<crate::providers::scopes::Scopes>,
        processed: tokio::sync::watch::Receiver<u64>,
        stopped: CancellationToken,
    ) -> Self {
        Self {
            activity: Default::default(),
            released: Default::default(),
            scopes,
            processed,
            stopped,
        }
    }
    pub(super) fn observe(&self, change: &AgentChange) {
        lock(&self.activity).observe(change, Instant::now());
    }
    pub(super) fn touch(&self, id: &str) {
        lock(&self.activity).touch(id, Instant::now());
    }
    /// Resume before starting work when the native thread is unloaded or this
    /// Host released its subscription; a loaded thread alone sends no events.
    pub(super) async fn start(
        &self,
        id: &str,
        reload: bool,
        resume: impl Future<Output = Result<(), Failure>>,
    ) -> Result<(), Failure> {
        let mut released = self.released.lock().await;
        let subscribed = lock(&self.activity)
            .threads
            .get(id)
            .is_some_and(|thread| thread.subscribed);
        if reload || released.contains(id) || !subscribed {
            resume.await?;
            released.remove(id);
        }
        self.touch(id);
        Ok(())
    }
    /// An idle release skips a thread that a start has resumed meanwhile.
    pub(super) async fn release(
        &self,
        process: &CodexAppServer,
        id: &str,
        only_idle: bool,
    ) -> Result<(), String> {
        let mut released = self.released.lock().await;
        if only_idle && !lock(&self.activity).releasable(id, Instant::now()) {
            return Ok(());
        }
        let reply = process
            .request_sequenced::<_, serde_json::Value>(
                "thread/unsubscribe",
                &serde_json::json!({"threadId":id}),
            )
            .await
            .map_err(|error| error.to_string())?;
        reply.value.outcome.map_err(|error| error.to_string())?;
        let mut processed = self.processed.clone();
        tokio::select! {
            result = processed.wait_for(|sequence| *sequence >= reply.sequence) => {result.map_err(|error|error.to_string())?;},
            _ = self.stopped.cancelled() => return Err("Codex event pump stopped".into()),
        }
        for source in lock(&self.activity).tree_ids(id) {
            self.scopes.forget(&source);
        }
        released.insert(id.to_owned());
        lock(&self.activity).forget(id);
        Ok(())
    }
    pub(super) fn spawn_releaser(self: Arc<Self>, process: Arc<CodexAppServer>) {
        let stopped = self.stopped.clone();
        tokio::spawn(async move {
            let mut check = tokio::time::interval(CHECK_INTERVAL);
            loop {
                tokio::select! {
                    _ = stopped.cancelled() => return,
                    _ = check.tick() => self.release_idle(&process).await,
                }
            }
        });
    }
    async fn release_idle(&self, process: &CodexAppServer) {
        let due = lock(&self.activity).due(Instant::now());
        for id in due {
            if let Err(error) = self.release(process, &id, true).await {
                tracing::warn!(target: "agent", operation = "host.codex.release_idle", thread = %id, message = %error);
                // Retry after another idle period instead of every check.
                self.touch(&id);
            }
        }
    }
}

fn lock<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

struct Thread {
    subscribed: bool,
    active: bool,
    running: HashSet<String>,
    parent: Option<String>,
    last: Instant,
}
impl Thread {
    fn new(now: Instant) -> Self {
        Self {
            subscribed: false,
            active: false,
            running: HashSet::new(),
            parent: None,
            last: now,
        }
    }
}

/// Native activity of loaded threads. Subagents are released with their root.
#[derive(Default)]
struct Activity {
    threads: HashMap<String, Thread>,
}
impl Activity {
    /// The Host subscribed to, or started work in, this thread.
    fn touch(&mut self, id: &str, now: Instant) {
        let thread = self.thread(id, now);
        thread.subscribed = true;
        thread.last = now;
    }
    fn observe(&mut self, change: &AgentChange, now: Instant) {
        match change {
            AgentChange::TaskParent { session, parent }
                if session.provider == Some(ProviderKind::Codex) =>
            {
                self.thread(&session.id, now).parent = Some(parent.id.clone());
            }
            AgentChange::Session { session, change }
                if session.provider == Some(ProviderKind::Codex) =>
            {
                let thread = self.thread(&session.id, now);
                thread.last = now;
                match change {
                    SessionChange::Status { status } => {
                        thread.active = *status == SessionStatus::Running;
                    }
                    SessionChange::Item { item, .. } if item.status == ItemStatus::Running => {
                        thread.running.insert(item.id.to_string());
                    }
                    SessionChange::Item { item, .. } => {
                        thread.running.remove(item.id.as_str());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    /// Subscribed root threads whose whole tree has been silent long enough.
    fn due(&self, now: Instant) -> Vec<String> {
        self.threads
            .iter()
            .filter(|(id, thread)| {
                thread.subscribed && thread.parent.is_none() && self.releasable(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
    fn releasable(&self, id: &str, now: Instant) -> bool {
        let tree = self.tree(id);
        let Some(last) = tree.iter().map(|thread| thread.last).max() else {
            return false;
        };
        let silence = now.saturating_duration_since(last);
        let pinned = tree.iter().any(|thread| !thread.running.is_empty());
        !tree.iter().any(|thread| thread.active) && silence >= if pinned { MAX_PIN } else { IDLE }
    }
    fn forget(&mut self, id: &str) {
        let ids = self.tree_ids(id);
        self.threads.retain(|id, _| !ids.contains(id));
    }
    fn thread(&mut self, id: &str, now: Instant) -> &mut Thread {
        self.threads
            .entry(id.to_owned())
            .or_insert_with(|| Thread::new(now))
    }
    fn tree(&self, id: &str) -> Vec<&Thread> {
        self.tree_ids(id)
            .iter()
            .filter_map(|id| self.threads.get(id))
            .collect()
    }
    fn tree_ids(&self, root: &str) -> HashSet<String> {
        let mut ids = HashSet::from([root.to_owned()]);
        // Invalid ancestry cannot grow the set forever.
        loop {
            let children: Vec<_> = self
                .threads
                .iter()
                .filter(|(id, thread)| {
                    !ids.contains(*id)
                        && thread
                            .parent
                            .as_ref()
                            .is_some_and(|parent| ids.contains(parent))
                })
                .map(|(id, _)| id.clone())
                .collect();
            if children.is_empty() {
                return ids;
            }
            ids.extend(children);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        items::{ItemBody, ItemContent},
        models::Item,
        session::SessionRef,
    };
    use std::sync::Arc;

    fn session(id: &str) -> SessionRef {
        SessionRef::new(ProviderKind::Codex, id.into()).unwrap()
    }
    fn status(id: &str, status: SessionStatus) -> AgentChange {
        AgentChange::Session {
            session: session(id),
            change: SessionChange::Status { status },
        }
    }
    fn item(id: &str, status: ItemStatus) -> AgentChange {
        AgentChange::Session {
            session: session(id),
            change: SessionChange::Item {
                turn_id: "turn".into(),
                item: Arc::new(Item {
                    id: "command".into(),
                    status,
                    client_input_id: None,
                    body: ItemContent::Inline {
                        body: Box::new(ItemBody::UserMessage {
                            text: None,
                            content: Vec::new(),
                        }),
                    },
                }),
            },
        }
    }

    #[tokio::test]
    async fn a_restored_thread_resumes_once_and_a_failed_resume_remains_retryable() {
        let subscriptions = Subscriptions::new(
            Arc::default(),
            tokio::sync::watch::channel(0).1,
            CancellationToken::new(),
        );
        assert!(
            subscriptions
                .start("restored", false, async {
                    Err(Failure::new("provider_unavailable", "resume failed"))
                })
                .await
                .is_err()
        );
        let mut resumed = false;
        subscriptions
            .start("restored", false, async {
                resumed = true;
                Ok(())
            })
            .await
            .unwrap();
        assert!(resumed);
        subscriptions
            .start("restored", false, async {
                panic!("a subscribed thread must not resume twice")
            })
            .await
            .unwrap();
        subscriptions.touch("created");
        subscriptions
            .start("created", false, async {
                panic!("thread/start already established this subscription")
            })
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn releasing_a_source_drains_older_frames_before_a_new_reservation() {
        tokio::time::timeout(Duration::from_secs(5), async {
        use agent_protocol::models::{ThreadResponse, Turn};
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("subscription-fixture");
        std::fs::write(&program, r#"#!/usr/bin/env node
const rl = require('node:readline').createInterface({input:process.stdin});
rl.on('line', line => {
  const m=JSON.parse(line); if(m.id === undefined) return;
  const result=m.method==='initialize' ? {userAgent:'subscription-fixture',codexHome:process.env.CODEX_HOME} : {};
  process.stdout.write(JSON.stringify({id:m.id,result})+'\n');
});
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let process = Arc::new(
            CodexAppServer::spawn(super::super::Config {
                program,
                codex_home: Some(directory.path().into()),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let mut events = process.subscribe();
        let scopes = Arc::new(crate::providers::scopes::Scopes::default());
        let first = uuid::Uuid::new_v4();
        let next = uuid::Uuid::new_v4();
        scopes.reserve("native", first);
        let started = |id: &str| AgentChange::Session {
            session: session("native"),
            change: SessionChange::Turn {
                turn: Turn {
                    id: id.into(),
                    ..Default::default()
                },
                completed: false,
            },
        };
        assert_eq!(scopes.observe(&started("old")), Some(first));
        let (processed, progress) = tokio::sync::watch::channel(0);
        let subscriptions = Arc::new(Subscriptions::new(
            scopes.clone(),
            progress,
            CancellationToken::new(),
        ));
        subscriptions.observe(&AgentChange::TaskParent {session:session("child"),parent:session("native")});
        scopes.admit("child");
        let child_frame = AgentChange::Session {session:session("child"),change:SessionChange::Turn {turn:Turn {id:"child-turn".into(),..Default::default()},completed:false}};
        assert!(scopes.observe(&child_frame).is_some());
        let mut release = Box::pin(subscriptions.release(&process,"native",false));
        let sequence = loop {
            tokio::select! {
                result = &mut release => panic!("release completed before older frames were drained: {result:?}"),
                event = events.recv() => if let agent_transport::peer::PeerEvent::Response {sequence,..} = event.unwrap() {break sequence;},
            }
        };
        assert!(futures_util::poll!(&mut release).is_pending(),"native acknowledgement alone does not mean older callbacks were applied");
        let mut resume = Box::pin(subscriptions.start("native", false, async {
            scopes.history(&ThreadResponse {
                thread: agent_protocol::models::Thread {
                    id: Some(session("native")),
                    turns: Some(vec![Arc::new(Turn {
                        id: "old".into(),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            });
            scopes.reserve("native", next);
            Ok(())
        }));
        assert!(futures_util::poll!(&mut resume).is_pending());
        assert_eq!(scopes.observe(&started("old")), Some(first));
        processed.send_replace(sequence);
        release.await.unwrap();
        assert!(scopes.observe(&child_frame).is_none(),"releasing a root also releases its child correlation scopes");
        resume.await.unwrap();
        assert_eq!(scopes.observe(&started("old")), None);
        assert_eq!(scopes.observe(&started("new")), Some(next));
        }).await.expect("native subscription release and resume settle");
    }

    #[test]
    fn an_idle_subscription_is_released_after_the_idle_timeout() {
        let start = Instant::now();
        let mut activity = Activity::default();
        activity.touch("root", start);
        activity.observe(&status("root", SessionStatus::Running), start);
        activity.observe(&status("root", SessionStatus::Idle), start);
        assert!(
            activity
                .due(start + IDLE - Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(activity.due(start + IDLE), ["root"]);
        activity.forget("root");
        assert!(activity.due(start + MAX_PIN).is_empty());
    }

    #[test]
    fn running_turns_and_waiting_requests_are_never_released() {
        let start = Instant::now();
        let mut activity = Activity::default();
        activity.touch("root", start);
        activity.observe(&status("root", SessionStatus::Running), start);
        assert!(activity.due(start + MAX_PIN * 2).is_empty());
    }

    #[test]
    fn unsubscribed_threads_are_not_released() {
        let start = Instant::now();
        let mut activity = Activity::default();
        activity.observe(&status("other", SessionStatus::Idle), start);
        assert!(activity.due(start + MAX_PIN).is_empty());
    }

    #[test]
    fn background_work_pins_until_it_has_been_silent_for_the_pin_limit() {
        let start = Instant::now();
        let mut activity = Activity::default();
        activity.touch("root", start);
        activity.observe(&item("root", ItemStatus::Running), start);
        activity.observe(&status("root", SessionStatus::Idle), start);
        assert!(activity.due(start + IDLE).is_empty());
        // Reports from the work restart the pin budget.
        let report = start + MAX_PIN - Duration::from_secs(1);
        activity.observe(&status("root", SessionStatus::Idle), report);
        assert!(activity.due(start + MAX_PIN).is_empty());
        assert_eq!(activity.due(report + MAX_PIN), ["root"]);
        // Finished work leaves only the idle timeout.
        activity.observe(&item("root", ItemStatus::Completed), report);
        assert_eq!(activity.due(report + IDLE), ["root"]);
    }

    #[test]
    fn subagents_keep_their_root_and_are_released_only_with_it() {
        let start = Instant::now();
        let mut activity = Activity::default();
        activity.touch("root", start);
        activity.observe(
            &AgentChange::TaskParent {
                session: session("child"),
                parent: session("root"),
            },
            start,
        );
        activity.touch("child", start);
        activity.observe(&status("child", SessionStatus::Running), start);
        assert!(activity.due(start + IDLE * 2).is_empty());
        let finished = start + IDLE;
        activity.observe(&status("child", SessionStatus::Idle), finished);
        assert!(
            activity
                .due(finished + IDLE - Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(activity.due(finished + IDLE), ["root"]);
        activity.forget("root");
        assert!(activity.threads.is_empty());
    }

    #[test]
    fn cyclic_ancestry_terminates() {
        let start = Instant::now();
        let mut activity = Activity::default();
        for (child, parent) in [("a", "b"), ("b", "a")] {
            activity.observe(
                &AgentChange::TaskParent {
                    session: session(child),
                    parent: session(parent),
                },
                start,
            );
        }
        assert_eq!(activity.tree_ids("a").len(), 2);
    }
}
