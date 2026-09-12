use host_fixture::{
    pairing::PairingServer,
    test_support::{HostFixture, Memory},
};
use std::{path::PathBuf, sync::Arc};

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
    let fixture = PathBuf::from(
        std::env::args_os()
            .nth(2)
            .expect("Codex fixture executable required"),
    );
    let port_file = std::env::args_os()
        .nth(3)
        .expect("port output file required");
    let program = host_fixture::fixture::Config {
        trace: true,
        stream_delay_ms: std::env::args()
            .nth(4)
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
    let host = HostFixture::start(
        &directory,
        config,
        Arc::new(Memory::default()),
        "検証 Host",
        true,
    )
    .await
    .unwrap();
    let pairing = PairingServer::start(
        &directory,
        host.ticket.clone(),
        host.credentials.local_identity().await,
    )
    .unwrap();
    std::fs::write(port_file, pairing.port.to_string()).unwrap();
    println!("UI fixture ready: {}", directory.display());
    tokio::signal::ctrl_c().await.unwrap();
    pairing.shutdown().unwrap();
    host.close().await.unwrap();
}
