use crate::command_line::Mode;
use agent_core::{
    client::ReadHostStatus,
    state::operations::{CreateInvitation, RevokeDevice},
    transport::{Endpoint, Relays},
};
use anyhow::{Context, Result, bail};
use host_daemon::local_host::{LocalHostRegistry, LocalHostState};
use std::{path::PathBuf, time::Duration};

pub(crate) async fn run(mode: Mode, state_dir: Option<PathBuf>, isolated: bool) -> Result<()> {
    let (identity, ticket) = tokio::task::spawn_blocking(move || {
        let registry = if isolated {
            LocalHostRegistry::new(state_dir.clone().context("--state-dir required")?)
        } else {
            LocalHostRegistry::for_user()?
        };
        let host = registry.resolve(state_dir.as_deref().unwrap_or(registry.directory()))?;
        let ticket = match &host.state {
            LocalHostState::Ready(ticket) => ticket.clone(),
            LocalHostState::Stopped => bail!("Host is not running"),
            LocalHostState::Starting => bail!("Host is still starting; try again shortly"),
        };
        let identity = host.load_identity()?;
        Ok::<_, anyhow::Error>((identity, ticket))
    })
    .await??;
    // This client manages a Host on the same machine. It needs no public relay.
    let endpoint = Endpoint::bind(identity, Relays::Disabled).await?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let session = endpoint.connect(&ticket).await?;
        let (client, _events) = session.open_peer(Duration::from_secs(15), 8).await?;
        let result = match mode {
            Mode::Invite => client
                .call(&CreateInvitation {})
                .await
                .map(|value| serde_json::to_value(value).expect("invitation serializes")),
            Mode::Status => client
                .call(&ReadHostStatus {})
                .await
                .map(|value| serde_json::to_value(value).expect("Host status serializes")),
            Mode::Revoke { node_id } => client
                .call(&RevokeDevice { id: node_id })
                .await
                .map(|value| serde_json::to_value(value).expect("revoke result serializes")),
            _ => unreachable!("management command"),
        };
        client.close().await;
        session.close();
        result.map_err(anyhow::Error::from)
    })
    .await
    .context("timed out connecting to the local Host");
    endpoint.close().await;
    println!("{}", result??);
    Ok(())
}
