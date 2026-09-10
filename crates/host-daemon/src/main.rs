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

    if let Err(error) = runtime::run(config).await {
        agent_core::diagnostics::error("host.runtime", &error);
        eprintln!(
            "host daemon failed: {}",
            agent_core::diagnostics::sanitize(&error)
        );
        std::process::exit(1);
    }
    agent_core::diagnostics::shutdown();
}
