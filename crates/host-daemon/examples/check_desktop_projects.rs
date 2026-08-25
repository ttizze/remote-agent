use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::DesktopProjectStore;
use serde_json::{Map, Value, json};

#[tokio::main]
async fn main() {
    if let Err(error) = check().await {
        eprintln!("Codex Desktop project check failed: {error}");
        std::process::exit(1);
    }
}

async fn check() -> Result<(), Box<dyn std::error::Error>> {
    let store = DesktopProjectStore::from_environment()?;
    let result = store.project_list(&json!({"limit": 512})).await?;
    let project_count = result
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    let app_server = CodexAppServer::spawn(AppServerConfig::default()).await?;
    let threads = app_server
        .request_json("thread/list", json!({"limit": 512}), Map::new())
        .await?;
    let upstream_assigned = assigned_thread_count(&threads);
    let enriched = store.enrich_threads(threads).await?;
    let thread_count = enriched
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let desktop_assigned = assigned_thread_count(&enriched);
    app_server.shutdown().await?;

    println!(
        "Codex Desktop project state is readable: {project_count} local project(s), \
         {desktop_assigned}/{thread_count} listed thread(s) assigned after Desktop overlay \
         ({upstream_assigned} from App Server), source {}",
        store.path().display(),
    );
    Ok(())
}

fn assigned_thread_count(result: &Value) -> usize {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|thread| thread.get("projectId").is_some_and(Value::is_string))
        .count()
}
