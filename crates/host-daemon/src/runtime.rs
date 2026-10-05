use crate::command_line::StartupConfig;
use agent_transport::transport::{Endpoint, Relays};
use anyhow::{Context, Result};
use host_daemon::{
    FileKeyStore, HostCredentials, HostRpcService, HostRuntime, ProjectStore,
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
            )
        } else {
            let registry = LocalHostRegistry::for_user()?;
            let directory = match config.state_dir {
                Some(directory) => directory,
                None => registry.resolve(registry.directory())?.directory,
            };
            registry.acquire(&directory)
        }
    })
    .await
    .context("Host lease worker failed")?
    .context("cannot acquire Host lease")?;
    let directory = lease.directory().to_owned();
    agent_transport::diagnostics::initialize(
        &directory,
        agent_transport::diagnostics::Component::Host,
        env!("CARGO_PKG_VERSION"),
    )
    .context("cannot initialize Host error log")?;
    let store = Arc::new(FileKeyStore(directory.join("identity.keys")));
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
    let projects = ProjectStore::new(directory.join("bex-worktrees.json"));
    let account_directory = config.account_state_dir.as_deref().unwrap_or(&directory);
    let service =
        HostRpcService::new(projects).context("cannot initialize conversation runtime")?;
    service
        .configure_providers(
            account_directory.to_owned(),
            config.codex,
            config.codex_home,
            config.claude,
            config.claude_home,
        )
        .await
        .context("cannot initialize provider registry")?;
    #[cfg(unix)]
    service
        .enable_browser(directory.join("browser"))
        .await
        .map_err(anyhow::Error::msg)?;
    service.start();
    let local_ticket = endpoint.local_ticket();
    let runtime = Arc::new(
        HostRuntime::new(
            service,
            endpoint,
            credentials,
            config.name,
            std::time::Duration::from_secs(config.invitation_days * 24 * 60 * 60),
        )
        .await,
    );
    let lease = tokio::task::spawn_blocking(move || {
        lease.publish(&local_ticket)?;
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
    drop(lease);
    result
}
