use crate::command_line::StartupConfig;
use agent_local_host::{HostLease, LocalHostRegistry};
use agent_transport::transport::{Endpoint, Relays};
use anyhow::{Context, Result};
use host_daemon::{FileKeyStore, HostCredentials, HostRpcService, HostRuntime, ProjectStore};
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
        option_env!("AGENT_BUILD_REVISION").unwrap_or("development"),
    )
    .context("cannot initialize Host error log")?;
    let store = Arc::new(FileKeyStore(agent_local_host::identity_path(&directory)));
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
    let endpoint = Endpoint::bind_host(credentials.host_identity().await, relays)
        .await
        .context("cannot bind Host endpoint")?;
    let account_directory = config.account_state_dir.as_deref().unwrap_or(&directory);
    let codex = host_daemon::providers::codex::Codex::spawn(
        host_daemon::providers::codex::Config {
            program: config.codex,
            codex_home: config.codex_home,
            ..Default::default()
        },
        account_directory.join("codex-accounts"),
    )
    .await;
    let claude = match host_daemon::providers::Claude::load(
        config.claude,
        account_directory.join("claude"),
        config.claude_home,
    )
    .await
    {
        Ok(adapter) => adapter.into(),
        Err(error) => {
            tracing::error!(target: "agent", operation = "host.claude", message = %error);
            host_daemon::providers::Backend::unavailable(
                agent_protocol::session::ProviderKind::Claude,
                error,
            )
        }
    };
    let deepseek = match host_daemon::providers::deepseek::DeepSeek::load(
        config.deepseek,
        account_directory.join("deepseek"),
        std::env::var_os("DSH_HOME")
            .map(Into::into)
            .or_else(|| directories::BaseDirs::new().map(|base| base.home_dir().join(".dsh"))),
        std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .map(zeroize::Zeroizing::new),
    )
    .await
    {
        Ok(adapter) => adapter.into(),
        Err(error) => host_daemon::providers::Backend::unavailable(
            agent_protocol::session::ProviderKind::DeepSeek,
            error,
        ),
    };
    let service = HostRpcService::new(
        [codex.into(), claude, deepseek],
        ProjectStore::new(directory.join("bex-worktrees.json")),
    );
    service.set_history_scope(directory.clone(), config.isolated);
    service
        .enable_apns(
            &directory.join("bex-apns.json"),
            &config.name,
            &endpoint.node_id().to_string(),
        )
        .await
        .context("cannot initialize Live Activity notifications")?;
    #[cfg(unix)]
    service
        .enable_browser(directory.join("browser"))
        .await
        .map_err(anyhow::Error::msg)?;
    service.start().map_err(anyhow::Error::msg)?;
    if let Err(error) = service.resume_after_update(&directory).await {
        tracing::error!(target: "agent", operation = "host.update.resume", message = %error);
    }
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
            .run(shutdown.clone(), std::time::Duration::from_secs(60 * 60));
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
