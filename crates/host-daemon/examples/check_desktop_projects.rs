use agent_core::{
    models::ThreadList,
    peer::{RpcResponse, request_line},
    state::operations::ListThreads,
};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{DesktopProjectStore, HostRpcService};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let projects = DesktopProjectStore::from_environment()?;
    let server = Arc::new(CodexAppServer::spawn(AppServerConfig::default()).await?);
    let service = HostRpcService::new(Ok(server.clone()), projects);
    let mut session = service.open_session(128);
    service
        .dispatch(
            session.id(),
            &agent_core::peer::RpcMessage::parse(&request_line(
                "host/thread/list",
                &ListThreads::new(Default::default()),
            )?)?,
        )
        .await?;
    loop {
        let line = session.recv().await.ok_or("Host session closed")?;
        if let Ok(response) = RpcResponse::<ThreadList>::parse(&line) {
            let list = response.outcome.map_err(|error| error.get().to_owned())?;
            println!(
                "Codex Desktop project state is readable: {} visible projects, {} visible conversations",
                list.projects.len(),
                list.data.len()
            );
            break;
        }
    }
    drop(session);
    drop(service);
    server.shutdown().await?;
    Ok(())
}
