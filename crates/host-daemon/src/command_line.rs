use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "host-daemon",
    no_binary_name = true,
    about = "Agent Host over authenticated iroh sessions"
)]
pub(crate) struct StartupConfig {
    #[command(subcommand)]
    pub(crate) mode: Option<Mode>,
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
    #[arg(long, default_value = "BEX Host")]
    pub(crate) name: String,
    /// Lifetime of newly issued one-use pairing invitations (1–90 days).
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u64).range(1..=90))]
    pub(crate) invitation_days: u64,
    /// Use only local addresses; intended for isolated fixtures.
    #[arg(long, conflicts_with = "relay_url")]
    pub(crate) no_relay: bool,
    /// Override the public iroh relay list.
    #[arg(long)]
    pub(crate) relay_url: Vec<String>,
}

#[derive(clap::Subcommand)]
pub(crate) enum Mode {
    /// Print a one-use pairing invitation as JSON (seven days by default).
    Invite,
    /// Print the running Host's identity, paired devices and provider errors as JSON.
    Status,
    /// Revoke a paired device and close its active connections.
    Revoke { node_id: String },
    #[command(hide = true)]
    BrowserMcp {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        thread: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_lifetime_defaults_to_a_week_and_is_bounded() {
        assert_eq!(
            StartupConfig::try_parse_from([] as [&str; 0])
                .unwrap()
                .invitation_days,
            7
        );
        assert_eq!(
            StartupConfig::try_parse_from(["--invitation-days", "90"])
                .unwrap()
                .invitation_days,
            90
        );
        for value in ["0", "91", "-1"] {
            assert!(StartupConfig::try_parse_from(["--invitation-days", value]).is_err());
        }
    }
}
