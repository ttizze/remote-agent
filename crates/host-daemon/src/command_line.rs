use clap::Parser;
use host_daemon::KeyStorage;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "host-daemon",
    no_binary_name = true,
    about = "Agent Host over authenticated iroh sessions"
)]
pub(crate) struct StartupConfig {
    #[arg(long, default_value = "codex")]
    pub(crate) codex: PathBuf,
    /// Claude Code executable. Uses the Host user's Claude subscription login.
    #[arg(long, default_value = "claude")]
    pub(crate) claude: PathBuf,
    /// Native Claude Code configuration and transcript storage root.
    #[arg(long)]
    pub(crate) claude_home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) codex_home: Option<PathBuf>,
    /// Credential directory; defaults to the remembered Host or platform data directory.
    #[arg(long)]
    pub(crate) state_dir: Option<PathBuf>,
    /// Shared provider account storage; defaults to the Host state directory.
    #[arg(long)]
    pub(crate) account_state_dir: Option<PathBuf>,
    /// Start a separate test/development Host, outside the user's shared instance.
    #[arg(long, requires = "state_dir")]
    pub(crate) isolated: bool,
    /// Defaults to the remembered backend, or keyring for a new Host.
    #[arg(long, value_enum)]
    pub(crate) key_storage: Option<KeyStorage>,
    #[arg(long, default_value = "BEX Host")]
    pub(crate) name: String,
    /// Use only local addresses; intended for isolated fixtures.
    #[arg(long, conflicts_with = "relay_url")]
    pub(crate) no_relay: bool,
    /// Override the public iroh relay list.
    #[arg(long)]
    pub(crate) relay_url: Vec<String>,
}
