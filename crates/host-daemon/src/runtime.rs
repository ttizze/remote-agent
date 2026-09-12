use crate::command_line::StartupConfig;
use agent_core::transport::{Endpoint, Relays};
use host_daemon::{
    CodexRpcService, DesktopProjectStore, HostCredentials, HostRuntime,
    local_host::{HostLease, LocalHostRegistry},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(config: StartupConfig) -> Result<(), String> {
    // Reserve the shared instance before provisioning keys or launching Codex.
    let lease = tokio::task::spawn_blocking(move || {
        if config.isolated {
            HostLease::isolated(
                config
                    .state_dir
                    .as_deref()
                    .ok_or("--isolated requires --state-dir")?,
                config.key_storage,
            )
        } else {
            let registry = LocalHostRegistry::for_user()?;
            let directory = match config.state_dir {
                Some(directory) => directory,
                None => registry.resolve(registry.directory())?.directory,
            };
            registry.acquire(&directory, config.key_storage)
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    let directory = lease.directory().to_owned();
    agent_core::diagnostics::initialize(
        &directory,
        agent_core::diagnostics::Component::Host,
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| format!("cannot initialize Host error log: {error}"))?;
    let store = lease.key_storage().open(&directory)?;
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
    let ticket = runtime.ticket();
    let lease = tokio::task::spawn_blocking(move || {
        lease.publish(&ticket)?;
        Ok::<_, String>(lease)
    })
    .await
    .map_err(|error| error.to_string())??;
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
    drop(lease);
    result
}
