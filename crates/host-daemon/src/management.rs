use crate::command_line::{Mode, StartupConfig};
use agent_local_host::{LocalHostRegistry, LocalHostState};
use agent_protocol::{
    operations::{ReadHostStatus, RevokeDevice},
    protocol::Call,
};
use agent_transport::transport::{Endpoint, Relays};
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::{path::PathBuf, time::Duration};

#[derive(Debug)]
pub(crate) struct DifferentHostBinary;
impl std::fmt::Display for DifferentHostBinary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The running WSL Host needs the bundled revision")
    }
}
impl std::error::Error for DifferentHostBinary {}

async fn resume_host(path: &std::path::Path) -> Result<()> {
    use std::process::{Command, Stdio};
    let launch: agent_protocol::operations::HostUpdate =
        serde_json::from_slice(&std::fs::read(path)?)?;
    let config = StartupConfig::try_parse_from(&launch.arguments)?;
    anyhow::ensure!(config.mode.is_none(), "Saved Host launch must start a Host");
    let state_dir = config.state_dir.map(|directory| {
        if directory.is_absolute() {
            directory
        } else {
            std::path::Path::new(&launch.working_directory).join(directory)
        }
    });
    let registry = if config.isolated {
        LocalHostRegistry::new(
            state_dir
                .clone()
                .context("isolated launch requires state directory")?,
        )
    } else {
        LocalHostRegistry::for_user()?
    };
    let preferred = state_dir.unwrap_or_else(|| registry.directory().to_owned());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let registry = registry.clone();
        let preferred = preferred.clone();
        let host = tokio::task::spawn_blocking(move || registry.resolve(&preferred)).await??;
        if matches!(host.state, LocalHostState::Stopped) {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "Previous Host did not stop"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(launch.arguments)
        .current_dir(launch.working_directory)
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000200);
    }
    println!("{}", command.spawn()?.id());
    Ok(())
}

pub(crate) async fn run(mode: Mode, state_dir: Option<PathBuf>, isolated: bool) -> Result<()> {
    if let Mode::Triage {
        output,
        description,
    } = &mode
    {
        let state = match state_dir {
            Some(state) => state,
            None => agent_local_host::LocalHostRegistry::for_user()?
                .directory()
                .to_owned(),
        };
        let prepared = host_daemon::triage::prepare(
            output,
            &state,
            &agent_protocol::triage::Prepare {
                source: None,
                description: description.clone(),
            },
            None,
        )?;
        println!("{}", serde_json::to_string_pretty(&prepared)?);
        return Ok(());
    }
    if let Mode::ResumeHost { launch_file } = &mode {
        return resume_host(launch_file).await;
    }
    let require_current = matches!(
        mode,
        Mode::LocalClient {
            require_current_binary: true,
            ..
        }
    );
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
        if require_current && !host.runs_executable(&std::env::current_exe()?)? {
            return Err(DifferentHostBinary.into());
        }
        let identity = agent_local_host::load_local_identity(&host.directory)?;
        Ok::<_, anyhow::Error>((identity, ticket))
    })
    .await??;
    if let Mode::LocalClient { address, .. } = mode {
        let access = agent_local_host::LocalClientAccess {
            ticket: ticket.with_ip(address),
            secret: identity.to_bytes(),
        };
        println!("{}", serde_json::to_string(&access)?);
        return Ok(());
    }
    // This client manages a Host on the same machine. It needs no public relay.
    let endpoint = Endpoint::bind(identity, Relays::Loopback).await?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let session = endpoint.connect(&ticket).await?;
        let (client, _events) = session.open_peer(Duration::from_secs(15), 8).await?;
        let result = match mode {
            Mode::PrepareUpdate => client
                .call(&agent_protocol::operations::PrepareHostUpdate {})
                .await
                .map(|value| serde_json::to_value(value).expect("Host launch serializes")),
            Mode::History {
                session,
                offset,
                limit,
                text_offset,
            } => client
                .call(&agent_protocol::handoff::ReadHistory {
                    session,
                    offset,
                    limit,
                    text_offset,
                })
                .await
                .map(|value| serde_json::to_value(value).expect("history serializes")),
            Mode::Invite => client
                .request::<agent_protocol::models::Invitation>(&Call::Invite(
                    agent_protocol::models::Empty {},
                ))
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
