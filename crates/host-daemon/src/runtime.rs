use crate::command_line::StartupConfig;
use agent_transport::transport::{Endpoint, Relays};
use anyhow::{Context, Result};
use host_daemon::{
    ConversationSettings, FileKeyStore, HostCredentials, HostRpcService, HostRuntime, ProjectStore,
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
    let app_server_config = codex_app_server::AppServerConfig {
        program: config.codex,
        codex_home: config.codex_home,
        ..Default::default()
    };
    let projects = ProjectStore::new(directory.join("bex-worktrees.json"));
    let app_server = codex_app_server::CodexAppServer::spawn(app_server_config.clone())
        .await
        .map(Arc::new)
        .map_err(|error| error.to_string());
    if let Err(error) = &app_server {
        tracing::error!(target: "bex", operation = "host.codex", message = %error);
    }
    let account_directory = config.account_state_dir.as_deref().unwrap_or(&directory);
    let service = HostRpcService::new(app_server.clone(), projects)?;
    #[cfg(unix)]
    service
        .enable_browser(directory.join("browser"))
        .await
        .map_err(anyhow::Error::msg)?;
    if let Err(error) = service
        .enable_claude(
            config.claude,
            account_directory.join("claude"),
            config.claude_home,
        )
        .await
    {
        tracing::error!(target:"bex", operation="host.claude", message=%error);
    }
    if app_server.is_ok()
        && let Err(error) = service
            .enable_accounts(
                account_directory.join("codex-accounts"),
                app_server_config.clone(),
            )
            .await
    {
        tracing::error!(target:"bex", operation="host.codex.accounts", message=%error);
    }
    service
        .enable_conversation(ConversationSettings {
            database: directory.join("conversation.sqlite"),
            codex: app_server_config.program.clone(),
            codex_home: app_server_config.codex_home.clone(),
        })
        .await
        .context("cannot open the conversation store")?;
    // Unfinished threads recover before the endpoint accepts any command.
    service
        .start()
        .await
        .context("cannot start the conversation runtime")?;
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
    if let Ok(app_server) = app_server {
        app_server
            .shutdown()
            .await
            .context("cannot shut down Codex app server")?;
    }
    drop(lease);
    result
}
