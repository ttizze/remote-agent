use crate::command_line::StartupConfig;
use agent_core::transport::{Endpoint, Relays};
use host_daemon::{
    CodexRpcService, CredentialStore, DesktopProjectStore, FileKeyStore, HostCredentials,
    HostRuntime, KeyringStore,
};
use std::{
    fs::{File, OpenOptions},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(config: StartupConfig) -> Result<(), String> {
    let directory = match config.state_dir {
        Some(path) => path,
        None => directories::ProjectDirs::from("app", "bex", "BEX")
            .ok_or("application data directory unavailable; provide --state-dir")?
            .data_local_dir()
            .to_path_buf(),
    };
    host_daemon::platform::create_state_directory(&directory).map_err(|e| e.to_string())?;
    let directory = directory.canonicalize().map_err(|e| e.to_string())?;
    // The standard library lock is held before loading or creating credentials.
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("host.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| e.to_string())?;
    let store: Arc<dyn CredentialStore> = match config.key_storage {
        crate::command_line::KeyStorage::Keyring => Arc::new(KeyringStore::new(
            directory.to_str().ok_or("state directory is not UTF-8")?,
        )?),
        crate::command_line::KeyStorage::File => {
            Arc::new(FileKeyStore(directory.join("identity.keys")))
        }
    };
    let credentials = Arc::new(HostCredentials::load(store, directory.clone()).await?);
    let relays = if config.no_relay {
        Relays::Disabled
    } else if config.relay_url.is_empty() {
        Relays::Default
    } else {
        Relays::Custom(config.relay_url)
    };
    let endpoint = Endpoint::bind(credentials.host_identity().await, relays)
        .await
        .map_err(|e| e.to_string())?;
    let app_server_config = codex_app_server::AppServerConfig {
        program: config.codex,
        codex_home: config.codex_home,
        ..Default::default()
    };
    let app_server = Arc::new(
        codex_app_server::CodexAppServer::spawn(app_server_config.clone())
            .await
            .map_err(|e| e.to_string())?,
    );
    let projects = DesktopProjectStore::new(
        app_server
            .initialize_response()
            .codex_home
            .join(".codex-global-state.json"),
    );
    let service = CodexRpcService::new(app_server.clone(), projects);
    service
        .enable_accounts(directory.join("codex-accounts"), app_server_config)
        .await?;
    service.start();
    let runtime = Arc::new(HostRuntime::new(service, endpoint, credentials, config.name).await);
    // This file contains a public endpoint address, never a private key or invitation.
    std::fs::write(directory.join("host.ticket"), runtime.ticket().to_string())
        .map_err(|e| e.to_string())?;
    let shutdown = CancellationToken::new();
    let result = {
        let run = runtime.clone().run(shutdown.clone());
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => result,
            signal = tokio::signal::ctrl_c() => {
                shutdown.cancel();
                let result = run.await;
                signal.map_err(|e| e.to_string()).and(result)
            }
        }
    };
    drop(runtime);
    let app_server =
        Arc::try_unwrap(app_server).map_err(|_| "active session retained Codex during shutdown")?;
    app_server.shutdown().await.map_err(|e| e.to_string())?;
    result
}
