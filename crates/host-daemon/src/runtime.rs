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
    // Reserve the shared instance before provisioning keys or launching providers.
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
    let account_directory = config.account_state_dir.as_deref().unwrap_or(&directory);
    let codex = host_daemon::adapters::codex::Codex::spawn(
        host_daemon::adapters::codex::Config {
            program: config.codex,
            codex_home: config.codex_home,
            ..Default::default()
        },
        account_directory.join("codex-accounts"),
    )
    .await;
    let claude = match host_daemon::adapters::Claude::load(
        config.claude,
        account_directory.join("claude"),
        config.claude_home,
    )
    .await
    {
        Ok(adapter) => adapter.into(),
        Err(error) => {
            tracing::error!(target: "bex", operation = "host.claude", message = %error);
            host_daemon::adapters::Backend::unavailable(
                agent_protocol::session::ProviderKind::Claude,
                error,
            )
        }
    };
    let service = HostRpcService::new(
        [codex.into(), claude],
        ProjectStore::new(directory.join("bex-worktrees.json")),
    );
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
    // The published ticket tells supervisors the Host is ready, so listen
    // first: an earlier interrupt would end the process without shutdown.
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .context("cannot receive shutdown signal")?;
    #[cfg(windows)]
    let mut interrupt =
        tokio::signal::windows::ctrl_c().context("cannot receive shutdown signal")?;
    let lease = tokio::task::spawn_blocking(move || {
        lease.publish(&local_ticket)?;
        Ok::<_, anyhow::Error>(lease)
    })
    .await
    .context("Host publication worker failed")?
    .context("cannot publish Host ticket")?;
    let shutdown = CancellationToken::new();
    let result = {
        let run = runtime
            .clone()
            .run(shutdown.clone(), std::time::Duration::from_secs(60));
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => result,
            _ = interrupt.recv() => {
                shutdown.cancel();
                run.await
            }
        }
    };
    drop(runtime);
    drop(lease);
    result
}
