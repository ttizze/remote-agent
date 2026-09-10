use agent_core::models::PageParams;
use agent_core::peer::raw_object;
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::DesktopProjectStore;
use host_daemon::ThreadPage;

#[tokio::main]
async fn main() {
    if let Err(error) = check().await {
        eprintln!("Codex Desktop project check failed: {error}");
        std::process::exit(1);
    }
}

async fn check() -> Result<(), Box<dyn std::error::Error>> {
    let store = DesktopProjectStore::from_environment()?;
    let result = store
        .project_list(&PageParams {
            limit: Some(512),
            ..Default::default()
        })
        .await?;
    let project_count = result.data.len();

    let app_server = CodexAppServer::spawn(AppServerConfig::default()).await?;
    let response = app_server
        .request_raw(r#"{"id":1,"method":"thread/list","params":{"limit":512}}"#)
        .await?;
    let object = raw_object(&response)?;
    let mut threads: ThreadPage = serde_json::from_str(object["result"].get())?;
    let upstream_assigned = assigned_thread_count(&threads);
    store.enrich_threads(&mut threads.data).await?;
    let thread_count = threads.data.len();
    let desktop_assigned = assigned_thread_count(&threads);
    app_server.shutdown().await?;

    println!(
        "Codex Desktop project state is readable: {project_count} local project(s), \
         {desktop_assigned}/{thread_count} listed thread(s) assigned after Desktop overlay \
         ({upstream_assigned} from App Server), source {}",
        store.path().display(),
    );
    Ok(())
}

fn assigned_thread_count(result: &ThreadPage) -> usize {
    result
        .data
        .iter()
        .filter(|thread| {
            thread
                .project_id
                .as_ref()
                .and_then(Option::as_ref)
                .is_some()
        })
        .count()
}
