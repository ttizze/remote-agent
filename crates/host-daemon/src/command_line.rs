use clap::Parser;
use std::path::PathBuf;

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum KeyStorage {
    Keyring,
    File,
}

#[derive(Parser)]
#[command(
    name = "host-daemon",
    no_binary_name = true,
    about = "Codex Host over authenticated iroh sessions"
)]
pub(crate) struct StartupConfig {
    #[arg(long, default_value = "codex")]
    pub(crate) codex: PathBuf,
    #[arg(long)]
    pub(crate) codex_home: Option<PathBuf>,
    /// Defaults to the platform's application data directory.
    #[arg(long)]
    pub(crate) state_dir: Option<PathBuf>,
    /// Use file storage on headless servers without an OS keyring service.
    #[arg(long, value_enum, default_value = "keyring")]
    pub(crate) key_storage: KeyStorage,
    #[arg(long, default_value = "BEX Host")]
    pub(crate) name: String,
    /// Use only local addresses; intended for isolated fixtures.
    #[arg(long, conflicts_with = "relay_url")]
    pub(crate) no_relay: bool,
    /// Override the public iroh relay list.
    #[arg(long)]
    pub(crate) relay_url: Vec<String>,
}
