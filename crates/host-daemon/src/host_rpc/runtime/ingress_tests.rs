use super::*;
use crate::providers::{
    Agent, Backend,
    codex::{Codex, Config},
};
use agent_protocol::models::{Thread, ThreadResponse};
use codex_app_server::CodexAppServer;
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicBool, Ordering},
};

// A real JSONL child, RpcPeer, Codex adapter and Host event pump. The child
// deliberately supplies no completed assistant snapshot: all chunks must make
// it through the receive/aggregate/persist path, independently of the producer.
fn program(root: &std::path::Path) -> std::path::PathBuf {
    let path = root.join("codex-wire-fixture");
    std::fs::write(&path,r##"#!/usr/bin/env node
const {createInterface}=require('node:readline');
const send=frame=>process.stdout.write(JSON.stringify(frame)+'\n');
const thread={id:'wire-thread',status:{type:'idle'},cwd:__dirname,turns:[]};
createInterface({input:process.stdin}).on('line',line=>{
 const message=JSON.parse(line);if(!('id' in message))return;
 if(message.method==='initialize')send({id:message.id,result:{userAgent:'fixture',codexHome:__dirname}});
 else if(message.method==='thread/read')send({id:message.id,result:{thread}});
 else if(message.method==='fixture/start'){
  send({method:'turn/started',params:{threadId:thread.id,turn:{id:'wire-turn',status:'inProgress',items:[]}}});
  send({method:'item/started',params:{threadId:thread.id,turnId:'wire-turn',item:{id:'wire-answer',type:'agentMessage',text:'',phase:'final_answer'}}});
  send({id:message.id,result:{}});
 }else if(message.method==='fixture/warm'){
  send({method:'item/agentMessage/delta',params:{threadId:thread.id,turnId:'wire-turn',itemId:'wire-answer',delta:'seed\n\n'}});
  send({id:message.id,result:{}});
 }else if(message.method==='fixture/burst'){
  for(let i=0;i<16000;i++)send({method:'item/agentMessage/delta',params:{threadId:thread.id,turnId:'wire-turn',itemId:'wire-answer',delta:'あ'+(i%10)}});
  send({method:'turn/completed',params:{threadId:thread.id,turn:{id:'wire-turn',status:'completed',items:[]}}});
  send({id:message.id,result:{}});
 }else send({id:message.id,result:{}});
});
"##).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[rstest::rstest]
#[case::cold(false)]
#[case::warm(true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_text_burst_survives_a_blocked_real_database(#[case] warm: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let server = Arc::new(
            CodexAppServer::spawn(Config {
                program: program(root.path()),
                codex_home: Some(root.path().join("home")),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let agent = Arc::new(Codex::new(Ok(server.clone())));
        let service = HostRpcService::new(
            [Backend {
                provider: ProviderKind::Codex,
                agent: Ok(agent.clone()),
                dictation: None,
            }],
            root.path().join("projects.json"),
        );
        let public = SessionRef::new(None, "public-conversation".into()).unwrap();
        let native = SessionRef::new(ProviderKind::Codex, "wire-thread".into()).unwrap();
        native_fixture::own(
            &service,
            &public,
            &ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(root.path().to_str().unwrap().into()),
                    turns: Some(vec![]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
        let _client = service.open_session();
        server
            .request::<_, Value>("fixture/start", &serde_json::json!({}))
            .await
            .unwrap();
        agent.state(&native.id).await.unwrap();
        if warm {
            server
                .request::<_, Value>("fixture/warm", &serde_json::json!({}))
                .await
                .unwrap();
            agent.state(&native.id).await.unwrap();
        }
        let (held, ready) = tokio::sync::oneshot::channel();
        let (release, unblock) = std::sync::mpsc::channel();
        let storage = service.inner.conversation.storage.clone();
        let blocker = tokio::task::spawn_blocking(move || storage.hold_connection(held, unblock));
        ready.await.unwrap();
        let (finish, finished) = std::sync::mpsc::channel();
        let forced = Arc::new(AtomicBool::new(false));
        let timed_out = forced.clone();
        let watchdog = std::thread::spawn(move || {
            if finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_err()
            {
                timed_out.store(true, Ordering::SeqCst);
            }
            let _ = release.send(());
        });
        let release_guard = scopeguard::guard(finish, |finish| {
            let _ = finish.send(());
        });
        let publication = if warm {
            let source = service.inner.conversation.resources.lock().unwrap()[&native].clone();
            let service = service.clone();
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let task = tokio::task::spawn_blocking(move || {
                let _updates = source.updates.blocking_lock();
                let _ = entered.send(());
                Runtime::flush_text(&service, &source).unwrap();
            });
            waiting.await.unwrap();
            Some(task)
        } else {
            None
        };
        // This RPC goes through the real transport but does not wait for Host
        // persistence. Its response is strictly after all 16000 notifications.
        let received = server
            .request::<_, Value>("fixture/burst", &serde_json::json!({}))
            .await;
        let independent_receive = !forced.load(Ordering::SeqCst);
        drop(release_guard);
        tokio::task::spawn_blocking(move || watchdog.join().unwrap())
            .await
            .unwrap();
        blocker.await.unwrap();
        if let Some(publication) = publication {
            publication.await.unwrap();
        }
        assert!(
            independent_receive,
            "DB contention must not starve wire reception"
        );
        received.unwrap();
        agent
            .state(&native.id)
            .await
            .expect("notification backlog must not terminate Codex");
        let saved = service
            .inner
            .conversation
            .storage
            .thread_state(&public.id, &[])
            .unwrap();
        let text = saved
            .turn_items
            .values()
            .find_map(|item| match &item.body {
                agent_protocol::orchestration::TurnItemBody::AssistantMessage { text, .. } => {
                    Some(text)
                }
                _ => None,
            })
            .unwrap();
        let expected = format!(
            "{}{}",
            if warm { "seed\n\n" } else { "" },
            (0..16000)
                .map(|i| format!("あ{}", i % 10))
                .collect::<String>()
        );
        assert_eq!(
            text, &expected,
            "every Unicode fragment must survive without a final snapshot or native reread"
        );
        assert!(
            saved
                .provider_turns
                .values()
                .all(|turn| turn.status.is_terminal())
        );
        assert!(agent.availability().is_ok());
        service.shutdown_owned_processes().await;
    })
    .await
    .expect("wire, persistence and shutdown must settle");
}
