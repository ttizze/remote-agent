use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::{Arc, Mutex}};
use host_daemon::{CodexRpcService, DesktopProjectStore, DeviceAuthenticationState, HostIdentity, HostRuntime, LocalListener, RemoteHosts, RemoteCredentialStore};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use tokio_util::sync::CancellationToken;
#[path="../../../tests/relay-e2e/phoenix.rs"] mod phoenix;
#[derive(Default)] struct Memory(Mutex<Option<zeroize::Zeroizing<Vec<u8>>>>);
impl RemoteCredentialStore for Memory {
    fn load(&self)->Result<Option<zeroize::Zeroizing<Vec<u8>>>,String>{Ok(self.0.lock().unwrap().clone())}
    fn save(&self,b:&[u8])->Result<(),String>{*self.0.lock().unwrap()=Some(zeroize::Zeroizing::new(b.to_vec()));Ok(())}
}
#[tokio::main]
async fn main() {
    let directory=PathBuf::from(std::env::args().nth(1).expect("isolated directory required"));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory,std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory=directory.canonicalize().unwrap();
    let workspace=directory.join("project"); std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("hello.txt"),"日本語のファイル\n編集を確認します。\n").unwrap();
    let nested=workspace.join("nested"); std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("child.txt"),"Nested directory navigation fixture.\n").unwrap();
    std::process::Command::new("git").args(["init","--quiet"]).current_dir(&workspace).status().unwrap();
    let state=directory.join("state");let listener=LocalListener::bind(&state).unwrap();
    let (mut relay,endpoint)=phoenix::start().await;
    let fixture=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/fake-codex-app-server.py").canonicalize().unwrap();
    let program=directory.join("codex-fixture.py");
    let source=format!("#!/usr/bin/env python3\nimport os,runpy\nos.environ.pop('BEX_FAKE_CODEX_TRACE',None)\nos.environ.pop('BEX_FAKE_CODEX_EXPECTED_CWD',None)\nos.environ['CODEX_HOME']={}\nrunpy.run_path({},run_name='__main__')\n",serde_json::to_string(&directory.to_string_lossy()).unwrap(),serde_json::to_string(&fixture.to_string_lossy()).unwrap());
    std::fs::write(&program,source).unwrap();std::fs::set_permissions(&program,std::fs::Permissions::from_mode(0o700)).unwrap();
    let projects=directory.join("projects.json");
    std::fs::write(&projects,serde_json::to_vec(&serde_json::json!({"local-projects":{"simulator-project":{"id":"simulator-project","name":"検証プロジェクト","rootPaths":[workspace],"createdAt":1,"updatedAt":1}},"project-order":["simulator-project"]})).unwrap()).unwrap();
    let server=Arc::new(codex_app_server::CodexAppServer::spawn(codex_app_server::AppServerConfig{program,..Default::default()}).await.unwrap());
    let service=CodexRpcService::new(server.clone(),DesktopProjectStore::new(projects));service.start();
    let key=Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let runtime=Arc::new(HostRuntime::new(service,HostIdentity::from_pkcs8(key.as_ref()).unwrap(),DeviceAuthenticationState::load(state.join("devices.json")).unwrap(),"検証 Mac".into(),endpoint,RemoteHosts::load(Arc::new(Memory::default())).unwrap()).unwrap());
    let stop=CancellationToken::new();let running=tokio::spawn(runtime.run(listener,stop.clone()));
    println!("UI fixture ready: {}",state.display());
    tokio::signal::ctrl_c().await.unwrap();stop.cancel();running.await.unwrap().unwrap();
    Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();relay.kill().await.unwrap();relay.wait().await.unwrap();
}
