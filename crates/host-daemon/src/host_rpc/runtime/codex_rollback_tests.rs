// The real Codex adapter and app-server transport, with only the native SDK
// process replaced by a strict recorded protocol peer. Expected requests come
// from T3's CodexAdapterV2 and CodexThreadRevert, not a Rust helper replica.
use crate::providers::{Agent, codex::{Codex, Config}};
use codex_app_server::CodexAppServer;
use serde_json::Value;
use std::sync::Arc;
use std::os::unix::fs::PermissionsExt;

#[rstest::rstest]
#[case("success", true, 1)]
#[case("not_loaded", true, 1)]
#[case("lost_ack", true, 1)]
#[case("legacy", false, 0)]
#[case("cycle", false, 0)]
#[case("missing", false, 0)]
#[case("wrong_thread", false, 1)]
#[tokio::test]
async fn rollback_uses_the_same_native_thread_and_a_stable_before_turn_boundary(
    #[case] mode: &str,
    #[case] succeeds: bool,
    #[case] expected_writes: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("codex-fixture");
    let log = directory.path().join("requests.jsonl");
    std::fs::write(directory.path().join("mode"), mode).unwrap();
    std::fs::write(&program,r##"#!/usr/bin/env node
const fs=require('node:fs'), path=require('node:path');
const mode=fs.readFileSync(path.join(__dirname,'mode'),'utf8');
let reverted=false;
const rl=require('node:readline').createInterface({input:process.stdin});
rl.on('line',line=>{
 const m=JSON.parse(line);if(m.id===undefined)return;
 fs.appendFileSync(path.join(__dirname,'requests.jsonl'),JSON.stringify(m)+'\n');
 let result;
 if(m.method==='initialize')result={userAgent:'rewind-fixture',codexHome:process.env.CODEX_HOME};
 else {
  if(m.params.threadId!=='source')process.exit(12);
  switch(m.method){
   case 'thread/read':
    if(m.params.includeTurns!==false)process.exit(13);
    result={thread:{id:'source',historyMode:mode==='legacy'?'legacy':'paginated',status:{type:mode==='not_loaded'?'notLoaded':'idle'}}};break;
   case 'thread/resume':
    if(m.params.cwd!=='/fixture')process.exit(14);
    result={thread:{id:'source',turns:[]}};break;
   case 'thread/turns/list':
    if(m.params.sortDirection!=='desc'||m.params.itemsView!=='summary'||m.params.limit<1||m.params.limit>100)process.exit(15);
    if(mode==='cycle')result={data:[{id:'unrelated'}],nextCursor:'same'};
    else if(mode==='missing')result={data:[{id:'unrelated'}],nextCursor:null};
    else if(reverted)result={data:[{id:'kept'},{id:'older'}],nextCursor:null};
    else if(m.params.cursor==null)result={data:[{id:'newest'},{id:'removed'}],nextCursor:'older'};
    else if(m.params.cursor==='older')result={data:[{id:'kept'},{id:'older'}],nextCursor:null};
    else process.exit(16);
    break;
   case 'thread/revert':
    if(m.params.beforeTurnId!=='removed'||reverted)process.exit(17);
    reverted=true;
    if(mode==='lost_ack'){
     process.stdout.write(JSON.stringify({id:m.id,error:{code:-32000,message:'write applied; acknowledgement lost'}})+'\n');return;
    }
    result={thread:{id:mode==='wrong_thread'?'other':'source'}};break;
   default:process.exit(18);
  }
 }
 process.stdout.write(JSON.stringify({id:m.id,result})+'\n');
});
"##).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = Arc::new(
        CodexAppServer::spawn(Config {
            program,
            codex_home: Some(directory.path().join("codex")),
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let agent = Codex::new(Ok(server));
    let mut result = agent
        .rollback("source", Some("kept"), Some("removed"), "/fixture", None)
        .await;
    if mode == "lost_ack" {
        assert!(
            result.is_err(),
            "lost acknowledgement cannot certify success"
        );
        result = agent
            .rollback("source", Some("kept"), Some("removed"), "/fixture", None)
            .await;
    }
    assert_eq!(
        result.is_ok(),
        succeeds,
        "{mode}: {:?}",
        result.as_ref().err().map(ToString::to_string)
    );
    if let Ok(result) = result {
        assert_eq!(result.native_id, "source");
        assert_eq!(result.head_id.as_deref(), Some("kept"));
    }
    let requests: Vec<Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        requests.iter().any(|m| m["method"] == "thread/read"),
        "the adapter must actually check native metadata, including rejection cases"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|m| m["method"] == "thread/revert")
            .count(),
        expected_writes
    );
    assert_eq!(
        requests
            .iter()
            .filter(|m| m["method"] == "thread/resume")
            .count(),
        usize::from(mode == "not_loaded")
    );
    assert!(
        requests
            .iter()
            .all(|m| !matches!(m["method"].as_str(), Some("thread/fork" | "thread/start")))
    );
    agent.shutdown().await;
}
