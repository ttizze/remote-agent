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
            snapshot.list_query = Arc::new(ListQuery {
                limit: *limit,
                project_limit: ListQuery::default().project_limit,
                search_term: search.clone(),
                ..Default::default()
            })
        }
        Command::Send { thread_id, .. } => {
            snapshot.navigation = Arc::new(Navigation {
                thread_id: Some(thread_id.clone()),
                draft_key: thread_id.clone().into(),
                ..Default::default()
            })
        }
        Command::Approve { .. } => {}
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
        Store::connect(&endpoint, &ticket, snapshot, args.invitation)
            .await
            .context("cannot connect to Host")?
    };
    let mut updates = store.subscribe();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            {
                let snapshot = updates.borrow_and_update();
                let ready = match &args.command {
                    Command::List { .. } => snapshot.threads.is_some(),
                    Command::Send { thread_id, .. } => {
                        snapshot.conversations.contains_key(thread_id)
                    }
                    Command::Approve { .. } => true,
                };
                if ready {
                    return Ok::<_, anyhow::Error>(());
                }
                if let Some(error) = &snapshot.error {
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
        Command::List { .. } => {
            println!("{}", serde_json::to_string(&store.snapshot().threads)?);
        }
        Command::Send {
            thread_id,
            text,
            client_message_id,
            model,
            effort,
        } => {
            store
                .dispatch(Intent::SetDraft {
                    thread_id: thread_id.clone().into(),
                    draft: Draft {
                        text,
                        model: model.map(|id| agent_protocol::models::ModelRef {
                            provider: thread_id.provider,
                            id,
                        }),
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
