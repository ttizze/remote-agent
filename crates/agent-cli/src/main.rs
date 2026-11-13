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
        /// Stable UUID for this submission, distinct from the conversation ID.
        #[arg(long)]
        client_message_id: uuid::Uuid,
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
            })
        }
        Command::Send { thread_id, .. } => {
            Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::new(Navigation {
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
        let device_name = tokio::task::spawn_blocking(agent_local_host::machine_name).await?;
        Store::connect(
            &endpoint,
            &ticket,
            snapshot,
            args.invitation,
            &device_name,
            None,
        )
        .await
        .context("cannot connect to Host")?
    };
    let mut updates = store.subscribe();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            {
                let snapshot = updates.borrow_and_update();
                let ready =
                    match &args.command {
                        Command::List { search, .. } => {
                            snapshot.lists.threads.is_some()
                                && snapshot.lists.replica.as_deref().is_some_and(
                                    agent_protocol::conversation::records::catalog_ready,
                                )
                                && (search.trim().is_empty()
                                    || snapshot
                                        .lists
                                        .search_results
                                        .as_ref()
                                        .is_some_and(|(term, _)| term == search))
                        }
                        Command::Send { thread_id, .. } => {
                            snapshot.sessions.conversations.contains_key(thread_id)
                        }
                        Command::Approve { .. } => true,
                    };
                if ready {
                    return Ok::<_, anyhow::Error>(());
                }
                if let Some(error) = &snapshot.panes.primary().error {
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
            println!(
                "{}",
                serde_json::to_string(&store.snapshot().lists.threads)?
            );
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
                .or(thread_id.provider)
                .context("cannot determine the conversation provider")?;
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
            let input = agent_protocol::ids::ClientInputId::from(client_message_id.to_string());
            let Outcome::Submitted = store
                .dispatch(Intent::Submit {
                    thread_id: Some(thread_id.clone()),
                    client_user_message_id: input.clone(),
                    alternate: false,
                })
                .await?
            else {
                unreachable!("submit outcome")
            };
            let turn =
                wait_for_submission(&mut updates, &thread_id, &input, Duration::from_secs(30))
                    .await?;
            println!("{}", serde_json::to_string(&turn)?);
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
            let request = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let Some(request) = updates
                        .borrow_and_update()
                        .request(request_id.as_str())
                        .cloned()
                    {
                        return Ok::<_, anyhow::Error>(request);
                    }
                    updates.changed().await?;
                }
            })
            .await
            .context("request could not be loaded")??;
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

async fn wait_for_submission(
    updates: &mut tokio::sync::watch::Receiver<Arc<Snapshot>>,
    thread_id: &agent_protocol::session::SessionRef,
    input: &agent_protocol::ids::ClientInputId,
    budget: Duration,
) -> anyhow::Result<Option<agent_protocol::ids::TurnId>> {
    use agent_protocol::session::{PreparationStatus, SubmissionDelivery};
    let result = tokio::time::timeout(budget, async {
        loop {
            {
                let snapshot = updates.borrow_and_update();
                if let Some(thread) = snapshot.sessions.conversations.get(thread_id) {
                    if let Some(preparation) = thread.preparations.iter().find(|preparation|&preparation.submission.client_user_message_id==input)
                        && let PreparationStatus::Failed {error} = &preparation.status {
                        return Err(anyhow::Error::msg(error.message.clone()));
                    }
                    match thread.submissions.get(input) {
                        Some(SubmissionDelivery::Accepted {turn_id}) => return Ok(turn_id.clone()),
                        Some(SubmissionDelivery::Unknown) => anyhow::bail!("input delivery result is unknown; resolve it in the conversation before retrying"),
                        Some(SubmissionDelivery::Rejected) => anyhow::bail!("input was rejected"),
                        Some(SubmissionDelivery::Withdrawn) => anyhow::bail!("input was withdrawn before it ran"),
                        _ => {},
                    }
                    if let Some(turn) = thread.turns.iter().flatten().find(|turn|
                        turn.items.iter().flatten().any(|item|item.client_input_id.as_ref()==Some(input))) {
                        return Ok(Some(turn.id.clone()));
                    }

                }
            }
            updates.changed().await?;
        }
    }).await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let snapshot = updates.borrow();
            match snapshot
                .sessions
                .conversations
                .get(thread_id)
                .and_then(|thread| thread.submissions.get(input))
            {
                Some(SubmissionDelivery::Sending) => Ok(None),
                Some(SubmissionDelivery::Unknown) => anyhow::bail!(
                    "input delivery result is unknown; resolve it in the conversation before retrying"
                ),
                _ => anyhow::bail!(
                    "could not confirm the input state; check the conversation before retrying"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        models::Thread,
        session::{ProviderKind, SessionRef, SubmissionDelivery},
    };

    #[tokio::test]
    async fn accepted_cli_input_reports_null_while_preparing_and_distinguishes_unknown_delivery() {
        for (delivery, expected) in [
            (SubmissionDelivery::Sending, Ok(None)),
            (SubmissionDelivery::Accepted { turn_id: None }, Ok(None)),
            (
                SubmissionDelivery::Accepted {
                    turn_id: Some("started".into()),
                },
                Ok(Some("started".into())),
            ),
            (
                SubmissionDelivery::Unknown,
                Err("delivery result is unknown"),
            ),
            (SubmissionDelivery::Rejected, Err("rejected")),
            (SubmissionDelivery::Withdrawn, Err("withdrawn")),
        ] {
            let id = SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap();
            let input: agent_protocol::ids::ClientInputId = uuid::Uuid::new_v4().to_string().into();
            let mut snapshot = Snapshot::default();
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                id.clone(),
                Arc::new(Thread {
                    id: Some(id.clone()),
                    submissions: [(input.clone(), delivery)].into(),
                    ..Default::default()
                }),
            );
            let (_sender, mut updates) = tokio::sync::watch::channel(Arc::new(snapshot));
            let result = wait_for_submission(&mut updates, &id, &input, Duration::ZERO).await;
            match expected {
                Ok(turn) => assert_eq!(result.unwrap(), turn),
                Err(message) => assert!(result.unwrap_err().to_string().contains(message)),
            }
        }
    }
}
