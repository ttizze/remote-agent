use codex_app_server::{AppServerConfig, CodexAppServer};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = CodexAppServer::spawn(AppServerConfig::default()).await?;
    let initialized = server.initialize_response();
    let threads: Value = serde_json::from_str(
        &server
            .request_raw(&serde_json::to_string(&json!({
                "id": "check-connection",
                "method": "thread/list",
                "params": { "limit": 1 },
            }))?)
            .await?,
    )?;

    println!(
        "connected to Codex on {}/{}; thread listing succeeded ({} row)",
        initialized.platform_family,
        initialized.platform_os,
        threads["result"]["data"].as_array().map_or(0, Vec::len)
    );

    server.shutdown().await?;
    Ok(())
}
