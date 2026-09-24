use agent_protocol::{models::ThreadList, operations::ListThreads};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{HostRpcService, ProjectStore};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let projects = ProjectStore::from_environment()?;
    let server = Arc::new(CodexAppServer::spawn(AppServerConfig::default()).await?);
    let service = HostRpcService::new(Ok(server.clone()), projects);
    let session = service.open_session(128);
    let reply = service
        .dispatch(
            session.id(),
            &agent_protocol::protocol::Call::ListThreads(ListThreads::new(Default::default())),
        )
        .await?;
    let response: agent_protocol::protocol::Response<ThreadList> =
        agent_protocol::protocol::decode(&reply.initial)?;
    let list = match response {
        agent_protocol::protocol::Response::Success { result } => result,
        agent_protocol::protocol::Response::Failure { error } => {
            return Err(error.to_string().into());
        }
    };
    println!(
        "Codex native project catalog is readable: {} visible projects, {} visible conversations",
        list.projects.len(),
        list.data.len()
    );
    drop(session);
    drop(service);
    server.shutdown().await?;
    Ok(())
}
