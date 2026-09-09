use clap::{Parser, builder::NonEmptyStringValueParser};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupConfig {
    pub(crate) relay: Option<host_protocol::RelayEndpoint>,
    pub(crate) configure: bool,
    pub(crate) codex: PathBuf,
    pub(crate) state_dir: Option<PathBuf>,
}

#[derive(Parser)]
#[command(
    name = "host-daemon",
    no_binary_name = true,
    about = "Owner-only host.sock and outbound encrypted relay sessions"
)]
struct Arguments {
    #[arg(
        long,
        help = "Read relayUrl, relayToken and runnerId as JSON from stdin and store in macOS Keychain"
    )]
    configure: bool,
    #[arg(long, requires_all = ["relay_token", "runner_id"], value_parser = NonEmptyStringValueParser::new())]
    relay_url: Option<String>,
    #[arg(long, requires_all = ["relay_url", "runner_id"], value_parser = NonEmptyStringValueParser::new())]
    relay_token: Option<String>,
    #[arg(long, requires_all = ["relay_url", "relay_token"], value_parser = NonEmptyStringValueParser::new())]
    runner_id: Option<String>,
    #[arg(long, value_name = "PATH", default_value = "codex", value_parser = NonEmptyStringValueParser::new())]
    codex: String,
    #[arg(long, value_name = "PATH", help = "State directory (default ~/.bex)", value_parser = NonEmptyStringValueParser::new())]
    state_dir: Option<String>,
}
impl StartupConfig {
    pub(crate) fn parse_from(
        arguments: impl IntoIterator<Item = String>,
    ) -> Result<Self, clap::Error> {
        let args = Arguments::try_parse_from(arguments)?;
        let relay = args
            .relay_url
            .map(|relay_url| host_protocol::RelayEndpoint {
                relay_url,
                relay_token: args.relay_token.expect("clap requires relay token"),
                runner_id: args.runner_id.expect("clap requires runner id"),
            });
        Ok(Self {
            relay,
            configure: args.configure,
            codex: args.codex.into(),
            state_dir: args.state_dir.map(PathBuf::from),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    fn parse(args: &[&str]) -> Result<StartupConfig, clap::Error> {
        StartupConfig::parse_from(args.iter().map(|value| (*value).into()))
    }
    #[test]
    fn preserves_relay_configuration_and_path_options() {
        let config = parse(&[
            "--relay-url",
            "wss://relay.example.test/socket/websocket",
            "--relay-token",
            "token",
            "--runner-id",
            "host",
            "--configure",
            "--codex",
            "/opt/bin/codex",
            "--state-dir",
            "/tmp/isolated host",
        ])
        .unwrap();
        assert_eq!(
            config.relay.unwrap(),
            host_protocol::RelayEndpoint {
                relay_url: "wss://relay.example.test/socket/websocket".into(),
                relay_token: "token".into(),
                runner_id: "host".into()
            }
        );
        assert!(config.configure);
        assert_eq!(config.codex, PathBuf::from("/opt/bin/codex"));
        assert_eq!(config.state_dir, Some(PathBuf::from("/tmp/isolated host")));
        assert!(parse(&[]).unwrap().relay.is_none());
        assert_eq!(parse(&[]).unwrap().codex, PathBuf::from("codex"));
    }
    #[test]
    fn rejects_incomplete_duplicate_empty_and_unknown_options() {
        for option in [
            "--relay-url",
            "--relay-token",
            "--runner-id",
            "--codex",
            "--state-dir",
        ] {
            assert!(parse(&[option]).is_err());
            assert!(parse(&[option, ""]).is_err());
            assert!(parse(&[option, "a", option, "b"]).is_err());
        }
        for args in [
            vec!["--relay-url", "ws://relay"],
            vec!["--relay-url", "ws://relay", "--relay-token", "token"],
        ] {
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                ErrorKind::MissingRequiredArgument
            );
        }
        assert_eq!(
            parse(&["--configure", "--configure"]).unwrap_err().kind(),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            parse(&["--listen", "127.0.0.1:49152"]).unwrap_err().kind(),
            ErrorKind::UnknownArgument
        );
        assert_eq!(parse(&["--help"]).unwrap_err().exit_code(), 0);
    }
}
