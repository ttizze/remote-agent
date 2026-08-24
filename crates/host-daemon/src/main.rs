mod command_line;
mod runtime;

use std::env;

use command_line::{ConfigError, StartupConfig, usage};

#[tokio::main]
async fn main() {
    let config = match StartupConfig::parse_from(env::args().skip(1)) {
        Ok(config) => config,
        Err(ConfigError::Help) => {
            println!("{}", usage());
            return;
        }
        Err(error) => {
            eprintln!("{error}\n\n{}", usage());
            std::process::exit(2);
        }
    };

    if let Err(error) = runtime::run(config).await {
        eprintln!("host daemon failed: {error}");
        std::process::exit(1);
    }
}
