use crate::command_line::StartupConfig;
use agent_core::transport::{Endpoint, Relays};
use anyhow::{Context, Result};
use host_daemon::{
    DesktopProjectStore, HostCredentials, HostRpcService, HostRuntime,
    local_host::{HostLease, LocalHostRegistry},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(config: StartupConfig) -> Result<()> {
    // Reserve the shared instance before provisioning keys or launching Codex.
    let lease = tokio::task::spawn_blocking(move || {
        if config.isolated {
            HostLease::isolated(
                config
                    .state_dir
                    .as_deref()
                    .context("--isolated requires --state-dir")?,
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
    .context("Host lease worker failed")?
    .context("cannot acquire Host lease")?;
    let directory = lease.directory().to_owned();
    agent_core::diagnostics::initialize(
        &directory,
        agent_core::diagnostics::Component::Host,
        env!("CARGO_PKG_VERSION"),
    )
    .context("cannot initialize Host error log")?;
    let store = lease
        .key_storage()
        .open(&directory)
        .context("cannot open Host key storage")?;
    let credentials = Arc::new(
        HostCredentials::load(store, directory.clone())
            .await
            .context("cannot load Host credentials")?,
    );
    let relays = if config.no_relay {
        Relays::Disabled
    } else if config.relay_url.is_empty() {
        Relays::Default
    } else {
        Relays::Custom(config.relay_url)
    };
    let endpoint = Endpoint::bind(credentials.host_identity().await, relays)
        .await
        .context("cannot bind Host endpoint")?;
    let app_server_config = codex_app_server::AppServerConfig {
        program: config.codex,
        codex_home: config.codex_home,
        ..Default::default()
    };
    let projects = match &app_server_config.codex_home {
        Some(home) => DesktopProjectStore::new(home.join(".codex-global-state.json")),
        None => DesktopProjectStore::from_environment().context("cannot locate project state")?,
    };
    let app_server = codex_app_server::CodexAppServer::spawn(app_server_config.clone())
        .await
        .map(Arc::new)
        .map_err(|error| error.to_string());
    if let Err(error) = &app_server {
        tracing::error!(target: "bex", operation = "host.codex", message = %error);
    }
    let service = HostRpcService::new(app_server.clone(), projects);
    service
        .enable_claude(config.claude, directory.join("claude"), config.claude_home)
        .await
        .context("cannot enable Claude")?;
    if app_server.is_ok() {
        service
            .enable_accounts(directory.join("codex-accounts"), app_server_config)
            .await
            .map_err(anyhow::Error::msg)
            .context("cannot enable Codex accounts")?;
    }
    service.start();
    let runtime = Arc::new(HostRuntime::new(service, endpoint, credentials, config.name).await);
    let ticket = runtime.ticket();
    let lease = tokio::task::spawn_blocking(move || {
        lease.publish(&ticket)?;
        Ok::<_, anyhow::Error>(lease)
    })
    .await
    .context("Host publication worker failed")?
    .context("cannot publish Host ticket")?;
    let shutdown = CancellationToken::new();
    let result = {
        let run = runtime.clone().run(shutdown.clone());
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => result,
            signal = tokio::signal::ctrl_c() => {
                shutdown.cancel();
                let result = run.await;
                signal.context("cannot receive shutdown signal").and(result)
            }
        }
    };
    drop(runtime);
    if let Ok(app_server) = app_server {
        app_server
            .shutdown()
            .await
            .context("cannot shut down Codex app server")?;
    }
    drop(lease);
    result
}
