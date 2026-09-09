mod command_line;
mod runtime;

use std::env;

use command_line::StartupConfig;

#[tokio::main]
async fn main() {
    let config = match StartupConfig::parse_from(env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => error.exit(),
    };

    if let Err(error) = runtime::run(config).await {
        eprintln!("host daemon failed: {error}");
        std::process::exit(1);
    }
}
