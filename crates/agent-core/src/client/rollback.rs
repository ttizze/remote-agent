//! A command ACK certifies admission. Native completion has its own canonical
//! witness, as in T3 ChatView.logic.waitForRevertedMessage. The scoped stream
//! outlives view navigation; only the normal view stream updates cached rows.
use super::{Client, PeerError, command_result, thread_frame};
use agent_protocol::{
    conversation::{Command, records},
    ids::TurnId,
    orchestration::{self as model, projection::*},
    session::SessionRef,
};
use std::collections::BTreeSet;

fn changed() -> PeerError {
    PeerError::InvalidMessage("The thread has changed. Select a message to rewind again.".into())
}
fn terminal(status: model::RunStatus) -> bool {
    matches!(
        status,
        model::RunStatus::Completed
            | model::RunStatus::Failed
            | model::RunStatus::Cancelled
            | model::RunStatus::Interrupted
    )
}

struct Witness {
    provider_thread: String,
    provider_instance: String,
    original_threads: BTreeSet<String>,
    retained: Option<u64>,
    removed_run: String,
}
impl Witness {
    fn new(projection: &ThreadProjection, run: &str) -> Result<Self, PeerError> {
        let removed = projection
            .runs
            .iter()
            .find(|candidate| candidate.id == run && terminal(candidate.status))
            .ok_or_else(changed)?;
        let retained = projection
            .runs
            .iter()
            .filter(|candidate| candidate.ordinal < removed.ordinal && terminal(candidate.status))
            .map(|run| run.ordinal)
            .max();
        let witness = projection
            .runs
            .iter()
            .filter(|run| run.ordinal > retained.unwrap_or(0) && terminal(run.status))
            .max_by_key(|run| run.ordinal)
            .ok_or_else(changed)?;
        let provider = projection
            .provider_threads
            .iter()
            .find(|thread| Some(&thread.id) == projection.thread.active_provider_thread_id.as_ref())
            .ok_or_else(changed)?;
        Ok(Self {
            provider_thread: provider.id.clone(),
            provider_instance: provider.provider_instance_id.clone(),
            original_threads: projection
                .provider_threads
                .iter()
                .map(|thread| thread.id.clone())
                .collect(),
            retained,
            removed_run: witness.id.clone(),
        })
    }
    fn confirmed(&self, projection: &ThreadProjection, request: &str) -> Result<bool, PeerError> {
        if let Some(failure) = projection
            .thread
            .rollback_failure
            .as_ref()
            .filter(|failure| failure.request_id == request)
        {
            return Err(PeerError::InvalidMessage(failure.message.clone()));
        }
        Ok(projection.provider_threads.iter().any(|thread| {
            (thread.id == self.provider_thread
                || (self.retained.is_none()
                    && thread.owner_node_id.is_none()
                    && thread.app_thread_id.as_deref() == Some(projection.thread.id.as_str())
                    && thread.provider_instance_id == self.provider_instance
                    && !self.original_threads.contains(&thread.id)))
                && thread.last_run_ordinal == self.retained
        }) && projection
            .runs
            .iter()
            .any(|run| run.id == self.removed_run && run.status == model::RunStatus::RolledBack))
    }
}

pub(crate) fn rewind<'a>(
    client: &'a Client,
    session: &'a SessionRef,
    run: &'a TurnId,
) -> impl std::future::Future<Output = Result<u64, PeerError>> + Send + 'a {
    Box::pin(async move {
        tokio::time::timeout(std::time::Duration::from_secs(120),async {
            let (opened,mut stream)=client.request_stream::<records::Opened>(&agent_protocol::protocol::Call::SubscribeConversation(records::Subscribe {
                session:session.clone(),subscription:uuid::Uuid::new_v4(),after_sequence:None,
            })).await?;
            if &opened.session!=session {return Err(changed());}
            let mut snapshot=None;
            for frame in opened.initial {
                snapshot=crate::orchestration::receive(&session.application_id(),snapshot.as_ref(),frame).map_err(|error|PeerError::InvalidMessage(error.into()))?;
            }
            let mut snapshot=snapshot.ok_or_else(changed)?;
            let witness=Witness::new(&snapshot.projection,run.as_str())?;
            let request=uuid::Uuid::new_v4();
            let request_id=request.to_string();
            let change=records::Change {session:session.clone(),operation:request,command:Command::Rewind(run.clone())};
            let reply=client.call(&change);
            tokio::pin!(reply);
            let mut accepted=None;
            let mut confirmed=false;
            loop {
                // Observe every frame even while the ACK is held. A queued
                // turn may advance the native head after this confirmed cut.
                confirmed|=witness.confirmed(&snapshot.projection,&request_id)?;
                if confirmed && let Some(sequence)=accepted {return Ok(sequence);}
                tokio::select! {
                    result=&mut reply, if accepted.is_none()=>accepted=Some(command_result(result?)?),
                    result=thread_frame(&mut snapshot,&mut stream)=>result?,
                }
            }
        }).await.unwrap_or_else(|_|Err(PeerError::RequestTimeout{method:"conversation rewind".into()}))
    })
}
