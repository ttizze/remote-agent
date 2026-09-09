use agent_core::transport::{Endpoint, Relays};
use host_daemon::{
    CodexRpcService, CredentialStore, DesktopProjectStore, HostCredentials, HostRuntime,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct Memory(Mutex<Option<zeroize::Zeroizing<Vec<u8>>>>);
impl CredentialStore for Memory {
    fn load(&self) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, b: &[u8]) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(zeroize::Zeroizing::new(b.to_vec()));
        Ok(())
    }
}
#[tokio::main]
async fn main() {
    let directory = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("isolated directory required"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let directory = directory.canonicalize().unwrap();
    let workspace = directory.join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("hello.txt"),
        "日本語のファイル\n編集を確認します。\n",
    )
    .unwrap();
    let nested = workspace.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("child.txt"),
        "Nested directory navigation fixture.\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&workspace)
        .status()
        .unwrap();
    let state = directory.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let credentials = Arc::new(
        HostCredentials::load(Arc::new(Memory::default()), state.clone())
            .await
            .unwrap(),
    );
    let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled)
        .await
        .unwrap();
    std::fs::write(state.join("host.ticket"), endpoint.ticket().to_string()).unwrap();
    std::fs::write(
        state.join("local.key"),
        credentials.local_identity().await.to_bytes(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            state.join("local.key"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let fixture = PathBuf::from(
        std::env::args_os()
            .nth(2)
            .expect("Codex fixture executable required"),
    );
    let program = xtask::fixture::Config {
        trace: true,
        stream_delay_ms: std::env::args()
            .nth(3)
            .map(|value| value.parse().expect("stream delay must be milliseconds"))
            .unwrap_or(8000),
        ..Default::default()
    }
    .install(&fixture, &directory)
    .unwrap();
    let projects = directory.join("projects.json");
    std::fs::write(&projects,serde_json::to_vec(&serde_json::json!({"local-projects":{"simulator-project":{"id":"simulator-project","name":"検証プロジェクト","rootPaths":[workspace],"createdAt":1,"updatedAt":1}},"project-order":["simulator-project"]})).unwrap()).unwrap();
    std::fs::write(directory.join("account-fixture.json"), r#"{"type":"chatgpt","email":"desktop@example.invalid","planType":"plus","accountId":"desktop"}"#).unwrap();
    let config = codex_app_server::AppServerConfig {
        program,
        ..Default::default()
    };
    let server = Arc::new(
        codex_app_server::CodexAppServer::spawn(config.clone())
            .await
            .unwrap(),
    );
    let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(projects));
    service
        .enable_accounts(state.join("accounts"), config)
        .await
        .unwrap();
    service.start();
    let runtime =
        Arc::new(HostRuntime::new(service, endpoint, credentials, "検証 Host".into()).await);
    let stop = CancellationToken::new();
    let running = tokio::spawn(runtime.run(stop.clone()));
    println!("UI fixture ready: {}", state.display());
    tokio::signal::ctrl_c().await.unwrap();
    stop.cancel();
    running.await.unwrap().unwrap();
    Arc::try_unwrap(server)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}
