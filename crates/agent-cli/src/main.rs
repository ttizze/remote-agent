use agent_core::peer::JsonlReader;
use agent_core::{
    client::Answer,
    models::ListQuery,
    peer::RpcPeer,
    state::{Draft, Intent, Navigation, Snapshot},
    store::{Outcome, Store},
    transport::{Endpoint, Identity, Relays, Ticket},
};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(about = "Headless agent client")]
struct Args {
    /// JSONL fixture executable, with stderr inherited for diagnostics.
    #[arg(long, required_unless_present = "ticket", conflicts_with = "ticket")]
    stdio: Option<String>,
    #[arg(long, requires = "stdio")]
    stdio_arg: Vec<String>,
    /// Endpoint ticket for an already paired Host.
    #[arg(long, requires = "identity_file")]
    ticket: Option<String>,
    /// Existing 32-byte client identity secret. Never printed.
    #[arg(long, requires = "ticket")]
    identity_file: Option<PathBuf>,
    /// One-use invitation for first pairing with this Host.
    #[arg(long, requires = "ticket")]
    invitation: Option<uuid::Uuid>,
    /// Disable relays and public address lookup for isolated local fixtures.
    #[arg(long, requires = "ticket")]
    no_relay: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    List {
        #[arg(long, default_value_t = 5)]
        project_limit: u32,
        #[arg(long, default_value_t = 5)]
        chat_limit: u32,
        #[arg(long, default_value = "")]
        search: String,
    },
    Send {
        thread_id: String,
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
        /// JSON request ID: a number or a quoted JSON string.
        #[arg(value_parser=parse_json)]
        request_id: Value,
        #[arg(long)]
        decision: u32,
    },
}

fn parse_json(value: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(value)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut snapshot = Snapshot::default();
    match &args.command {
        Command::List {
            project_limit,
            chat_limit,
            search,
        } => {
            snapshot.list_query = Arc::new(ListQuery {
                project_limit: *project_limit,
                chat_limit: *chat_limit,
                search_term: search.clone(),
                ..Default::default()
            })
        }
        Command::Send { thread_id, .. } => {
            snapshot.navigation = Arc::new(Navigation {
                thread_id: Some(thread_id.clone()),
                draft_key: thread_id.clone(),
                ..Default::default()
            })
        }
        Command::Approve { .. } => {}
    }
    let mut child = None;
    let store = if let Some(program) = args.stdio {
        let mut process = tokio::process::Command::new(program)
            .args(args.stdio_arg)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let peer = RpcPeer::open(
            JsonlReader::new(process.stdout.take().expect("piped stdout")),
            process.stdin.take().expect("piped stdin"),
            Some(Duration::from_secs(30)),
            64,
        )?;
        child = Some(process);
        Store::new(peer, snapshot)
    } else {
        let secret =
            tokio::fs::read(args.identity_file.expect("identity required with ticket")).await?;
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| "identity file must contain exactly 32 bytes")?;
        let endpoint = Endpoint::bind(
            Identity::from_bytes(bytes),
            if args.no_relay {
                Relays::Disabled
            } else {
                Relays::Default
            },
        )
        .await?;
        let ticket: Ticket = args.ticket.expect("connection required").parse()?;
        Store::connect(&endpoint, &ticket, snapshot, args.invitation).await?
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
                    return Ok::<_, Box<dyn std::error::Error>>(());
                }
                if let Some(error) = &snapshot.error {
                    return Err(error.clone().into());
                }
            }
            updates.changed().await?;
        }
    })
    .await??;
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
                    thread_id: thread_id.clone(),
                    draft: Draft {
                        text,
                        model,
                        effort,
                        ..Default::default()
                    },
                })
                .await?;
            let Outcome::Submitted { turn_id: id } = store
                .dispatch(Intent::Submit {
                    thread_id: Some(thread_id),
                    client_user_message_id: client_message_id,
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
            let mut updates = store.subscribe();
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if updates
                        .borrow_and_update()
                        .requests
                        .contains_key(&request_id.to_string())
                    {
                        return Ok::<_, Box<dyn std::error::Error>>(());
                    }
                    updates.changed().await?;
                }
            })
            .await??;
            store
                .dispatch(Intent::Respond {
                    request_id,
                    answer: Answer::Decision { index: decision },
                })
                .await?;
            println!("null");
        }
    }
    store.close().await?;
    if let Some(mut child) = child {
        child.kill().await?;
    }
    Ok(())
}
