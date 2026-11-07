use agent_core::{
    state::{Draft, Intent, Navigation, Snapshot, operations as op},
    store::{Outcome, Store},
};
use agent_protocol::{models::ListQuery, requests::Answer};
use agent_transport::transport::{Endpoint, Identity, Relays, Ticket};
use anyhow::Context;
use clap::{Parser, Subcommand};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(about = "Headless agent client")]
struct Args {
    /// Endpoint ticket for an already paired Host.
    #[arg(long)]
    ticket: String,
    /// Existing 32-byte client identity secret. Never printed.
    #[arg(long)]
    identity_file: PathBuf,
    /// One-use invitation for first pairing with this Host.
    #[arg(long)]
    invitation: Option<uuid::Uuid>,
    /// Disable relays and public address lookup for isolated local fixtures.
    #[arg(long)]
    no_relay: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Read the Host's current task states without starting an agent.
    Activity,
    /// Read the saved conversation without submitting input.
    History {
        thread_id: agent_protocol::session::SessionRef,
    },
    List {
        #[arg(long, default_value_t = ListQuery::default().limit)]
        limit: u32,
        #[arg(long, default_value = "")]
        search: String,
    },
    Send {
        thread_id: agent_protocol::session::SessionRef,
        text: String,
        /// Stable identity for this submission, distinct from the RPC ID.
        #[arg(long)]
        client_message_id: String,
        #[arg(long)]
        model: Option<String>,
        /// Switch provider within the same conversation on this submission.
        #[arg(long, requires = "model", value_parser = parse_provider)]
        provider: Option<agent_protocol::session::ProviderKind>,
        #[arg(long)]
        effort: Option<String>,
    },
    Approve {
        /// Opaque request ID issued by the Host.
        request_id: String,
        #[arg(long)]
        decision: u32,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!(
            "Error: {}",
            agent_transport::diagnostics::sanitize(&format!("{error:#}"))
        );
        std::process::exit(1);
    }
}

async fn run(args: Args) -> anyhow::Result<()> {
    let mut snapshot = Snapshot::default();
    match &args.command {
        Command::List { limit, search } => {
            snapshot.lists.list_query = Arc::new(ListQuery {
                limit: *limit,
                project_limit: ListQuery::default().project_limit,
                search_term: search.clone(),
                ..Default::default()
            })
        }
        Command::History { thread_id } | Command::Send { thread_id, .. } => {
            Arc::make_mut(&mut snapshot.pane).navigation = Arc::new(Navigation {
                thread_id: Some(thread_id.clone()),
                draft_key: thread_id.clone().into(),
                ..Default::default()
            })
        }
        Command::Activity | Command::Approve { .. } => {}
    }
    let store = {
        let secret = tokio::fs::read(args.identity_file)
            .await
            .context("cannot read client identity file")?;
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| anyhow::anyhow!("identity file must contain exactly 32 bytes"))?;
        let endpoint = Endpoint::bind(
            Identity::from_bytes(bytes),
            if args.no_relay {
                Relays::Disabled
            } else {
                Relays::Default
            },
        )
        .await
        .context("cannot bind client endpoint")?;
        let ticket: Ticket = args.ticket.parse().context("cannot parse Host ticket")?;
        let device_name = tokio::task::spawn_blocking(agent_local_host::machine_name).await?;
        Store::connect(&endpoint, &ticket, snapshot, args.invitation, &device_name)
            .await
            .context("cannot connect to Host")?
    };
    let mut updates = store.subscribe();
    // Keep the state that satisfied the read even if the Host then disconnects.
    let initial = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            {
                let snapshot = updates.borrow_and_update();
                let ready = match &args.command {
                    Command::Activity => snapshot.lists.task_activity.is_some(),
                    Command::List { .. } => snapshot.lists.threads.is_some(),
                    Command::History { thread_id } | Command::Send { thread_id, .. } => {
                        snapshot.sessions.conversations.contains_key(thread_id)
                    }
                    Command::Approve { .. } => true,
                };
                if ready {
                    return Ok::<_, anyhow::Error>(snapshot.clone());
                }
                if let Some(error) = &snapshot.pane.error {
                    return Err(anyhow::Error::msg(error.clone()));
                }
            }
            updates.changed().await?;
        }
    })
    .await
    .context("timed out waiting for initial Host state")?
    .context("cannot load initial Host state")?;
    match args.command {
        Command::Activity => {
            println!("{}", serde_json::to_string(&initial.lists.task_activity)?);
        }
        Command::History { thread_id } => {
            println!(
                "{}",
                serde_json::to_string(&initial.sessions.conversations[&thread_id])?
            );
        }
        Command::List { .. } => {
            println!("{}", serde_json::to_string(&initial.lists.threads)?);
        }
        Command::Send {
            thread_id,
            text,
            client_message_id,
            model,
            provider,
            effort,
        } => {
            let snapshot = store.snapshot();
            let key = thread_id.clone().into();
            let provider = provider
                .or_else(|| snapshot.model_provider_for_draft(key))
                .unwrap_or(thread_id.provider);
            store
                .dispatch(Intent::SetDraft {
                    thread_id: thread_id.clone().into(),
                    draft: Draft {
                        text,
                        model: model.map(|id| agent_protocol::models::ModelRef { provider, id }),
                        effort,
                        ..Default::default()
                    },
                })
                .await?;
            let Outcome::Submitted { turn_id: id } = store
                .dispatch(Intent::Submit {
                    thread_id: Some(thread_id),
                    client_user_message_id: client_message_id.into(),
                })
                .await?
            else {
                unreachable!("submit outcome")
            };
            println!("{}", serde_json::to_string(&id)?);
        }
        Command::Approve {
            request_id,
            decision,
        } => {
            store
                .dispatch(Intent::OpenRequest(op::OpenRequest {
                    request_id: request_id.clone().into(),
                }))
                .await?;
            let snapshot = store.snapshot();
            let request = snapshot
                .request(request_id.as_str())
                .ok_or_else(|| anyhow::anyhow!("request is unavailable"))?;
            let choice_id = request
                .body
                .choices()
                .get(decision as usize)
                .ok_or_else(|| anyhow::anyhow!("invalid request choice"))?
                .id
                .clone();
            let answer = match request.body {
                agent_protocol::requests::RequestBody::Approval { .. } => {
                    Answer::Approval { choice_id }
                }
                agent_protocol::requests::RequestBody::Permission { .. } => {
                    Answer::Permission { choice_id }
                }
                _ => anyhow::bail!("request does not offer approval choices"),
            };
            store
                .dispatch(Intent::Respond(op::Respond {
                    request_id: request_id.into(),
                    answer,
                }))
                .await?;
            println!("null");
        }
    }
    store.close().await?;
    Ok(())
}

fn parse_provider(value: &str) -> Result<agent_protocol::session::ProviderKind, String> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| "provider must be codex, claude or deepseek".into())
}
