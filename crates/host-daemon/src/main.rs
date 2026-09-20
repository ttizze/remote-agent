mod command_line;
mod runtime;

use std::env;

use clap::Parser;
use command_line::StartupConfig;

#[tokio::main]
async fn main() {
    let config = match StartupConfig::try_parse_from(env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => error.exit(),
    };

    if let Some(command_line::Mode::BrowserMcp { socket, thread }) = &config.mode {
        if let Err(error) = host_daemon::browser::mcp::serve(socket, thread).await {
            eprintln!("BEX browser bridge: {error}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(error) = runtime::run(config).await {
        let error = format!("{error:#}");
        tracing::error!(target: "bex", operation = "host.runtime", message = %error);
        eprintln!(
            "host daemon failed: {}",
            agent_core::diagnostics::sanitize(&error)
        );
        std::process::exit(1);
    }
    tracing::info!(target: "bex", operation = "shutdown", "Bex shutting down");
}
