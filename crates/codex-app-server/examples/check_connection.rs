use codex_app_server::{AppServerConfig, CodexAppServer};
use serde_json::{Map, json};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = CodexAppServer::spawn(AppServerConfig::default()).await?;
    let initialized = server.initialize_response();
    let threads = server
        .request_json("thread/list", json!({ "limit": 1 }), Map::new())
        .await?;

    println!(
        "connected to Codex on {}/{}; thread listing succeeded ({} row)",
        initialized.platform_family,
        initialized.platform_os,
        threads["data"].as_array().map_or(0, Vec::len)
    );

    server.shutdown().await?;
    Ok(())
}
