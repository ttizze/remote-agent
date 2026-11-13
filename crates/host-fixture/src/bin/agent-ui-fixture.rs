use agent_protocol::{
    browser::{BrowserAction, BrowserRequest},
    session::{ProviderKind, SessionRef},
};
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
    let directory = dunce::canonicalize(&directory).unwrap();
    // Opt in to the normal local-client discovery contract for native debugging.
    // The scoped lease and keys stay inside the explicitly supplied fixture root.
    let local =
        std::env::var_os("AGENT_FIXTURE_LOCAL_HOST").as_deref() == Some(std::ffi::OsStr::new("1"));
    let lease = local.then(|| agent_local_host::HostLease::isolated(&directory).unwrap());
    let memory = Arc::new(Memory::default());
    if local {
        use host_daemon::CredentialStore;
        match std::fs::read(agent_local_host::identity_path(&directory)) {
            Ok(bytes) => memory.save(&zeroize::Zeroizing::new(bytes)).unwrap(),
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => {}
            Err(reason) => panic!("fixture keys are unavailable: {reason}"),
        }
    }
    agent_transport::diagnostics::initialize(
        &directory,
        agent_transport::diagnostics::Component::Host,
        env!("CARGO_PKG_VERSION"),
        option_env!("AGENT_BUILD_REVISION").unwrap_or("development"),
    )
    .unwrap();
    println!("Preparing UI fixture workspace");
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
    let program = if fixture == std::path::Path::new("--without-codex") {
        directory.join("missing-codex")
    } else {
        host_fixture::fixture::Config {
            trace: true,
            stream_delay_ms: std::env::args()
                .nth(4)
                .map(|value| value.parse().expect("stream delay must be milliseconds"))
                .unwrap_or(8000),
            ..Default::default()
        }
        .install(&fixture, &directory)
        .unwrap()
    };
    std::fs::write(directory.join("account-fixture.json"), r#"{"type":"chatgpt","email":"desktop@example.invalid","planType":"plus","accountId":"desktop"}"#).unwrap();
    let config = codex_app_server::AppServerConfig {
        program,
        ..Default::default()
    };
    println!("Starting UI fixture Host");
    // After the Claude fixture: `--deepseek <DSH fixture>` and `--prepare-browser`.
    let options: Vec<_> = std::env::args_os().skip(6).collect();
    let deepseek = options
        .iter()
        .position(|option| option == "--deepseek")
        .map(|index| PathBuf::from(options.get(index + 1).expect("--deepseek needs a path")));
    let prepare_browser = options.iter().any(|option| option == "--prepare-browser");
    let host = HostFixture::start(
        &directory,
        config,
        memory.clone(),
        "検証 Host",
        true,
        std::env::args_os().nth(5).map(PathBuf::from).as_deref(),
        deepseek.as_deref(),
    )
    .await
    .unwrap();
    host.projects.replace_fixture_projects(&serde_json::from_value::<Vec<agent_protocol::models::Project>>(serde_json::json!([
        {"id":"simulator-project","name":"検証プロジェクト","roots":[{"path":workspace}],"createdAt":1,"updatedAt":1}
    ])).unwrap()).unwrap();
    if let Some(lease) = &lease {
        use host_daemon::CredentialStore;
        agent_local_host::save_private_bytes(
            &agent_local_host::identity_path(&directory),
            memory.load().unwrap().as_ref().unwrap(),
        )
        .unwrap();
        lease.publish(&host.ticket).unwrap();
    }
    #[cfg(target_os = "macos")]
    let mut browser_desktop = None;
    if prepare_browser {
        println!("Preparing native browser dependency");
        // Use the production native owner in an isolated Mac app window: the
        // bex.app `scripts/build-macos.sh desktop dev` packages beside this
        // fixture. The fixture's authenticated Host RPC reaches exactly that renderer.
        #[cfg(target_os = "macos")]
        {
            let executable = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("bex.app/Contents/MacOS/bex");
            let mut child = tokio::process::Command::new(executable)
                .env("AGENT_STATE_DIR", &directory)
                .env("AGENT_ISOLATED_HOST", "1")
                .env("AGENT_HOST_DAEMON", "/usr/bin/false")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while agent_local_host::browser::read_renderer(&directory).is_err() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "native browser desktop exited during startup"
                );
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "native browser renderer did not become ready"
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            browser_desktop = Some(child);
        }
        let connection = host.local().await.unwrap();
        let ready = connection
            .peer
            .call(&BrowserRequest {
                thread_id: SessionRef::new(ProviderKind::Codex, "browser-preflight".into())
                    .unwrap(),
                tab_id: String::new(),
                image_id: String::new(),
                renderer_id: String::new(),
                generation: 0,
                action: BrowserAction::Read,
            })
            .await;
        connection.close().await;
        ready.expect("isolated browser dependency must be ready before Simulator startup");
        println!("Browser dependency ready");
    }
    let pairing = PairingServer::start(
        &directory,
        host.projects.clone(),
        host.ticket.clone(),
        host.credentials.local_identity().await,
    )
    .unwrap();
    std::fs::write(port_file, pairing.port.to_string()).unwrap();
    println!("UI fixture ready: {}", directory.display());
    tokio::signal::ctrl_c().await.unwrap();
    pairing.shutdown().unwrap();
    host.close().await.unwrap();
    #[cfg(target_os = "macos")]
    if let Some(mut child) = browser_desktop {
        let _ = child.kill().await;
    }
}
