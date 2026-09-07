use std::path::PathBuf;

const DEFAULT_CODEX: &str = "codex";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupConfig {
    pub(crate) relay: Option<host_protocol::RelayEndpoint>,
    pub(crate) configure: bool,
    pub(crate) codex: PathBuf,
    pub(crate) state_dir: Option<PathBuf>,
}

impl StartupConfig {
    pub(crate) fn parse_from(
        arguments: impl IntoIterator<Item = String>,
    ) -> Result<Self, ConfigError> {
        let mut configure = None;
        let mut relay_url = None;
        let mut relay_token = None;
        let mut runner_id = None;
        let mut codex = None;
        let mut state_dir = None;
        let mut arguments = arguments.into_iter();

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--help" | "-h" => return Err(ConfigError::Help),
                "--configure" => set_once(&mut configure, "--configure", true)?,
                "--relay-url" => set_once(
                    &mut relay_url,
                    "--relay-url",
                    next_non_empty_value(&mut arguments, "--relay-url")?,
                )?,
                "--relay-token" => set_once(
                    &mut relay_token,
                    "--relay-token",
                    next_non_empty_value(&mut arguments, "--relay-token")?,
                )?,
                "--runner-id" => set_once(
                    &mut runner_id,
                    "--runner-id",
                    next_non_empty_value(&mut arguments, "--runner-id")?,
                )?,
                "--state-dir" => set_once(&mut state_dir, "--state-dir", next_non_empty_value(&mut arguments, "--state-dir")?)?,
                "--codex" => set_once(
                    &mut codex,
                    "--codex",
                    next_non_empty_value(&mut arguments, "--codex")?,
                )?,
                _ => return Err(ConfigError::UnknownArgument(argument)),
            }
        }

        let relay = if relay_url.is_none() && relay_token.is_none() && runner_id.is_none() { None } else {
            Some(host_protocol::RelayEndpoint {
                relay_url: relay_url.ok_or(ConfigError::MissingRelayUrl)?,
                relay_token: relay_token.ok_or(ConfigError::MissingRelayToken)?,
                runner_id: runner_id.ok_or(ConfigError::MissingRunnerId)?,
            })
        };
        Ok(Self {
            relay,
            configure: configure.unwrap_or(false),
            state_dir: state_dir.map(PathBuf::from),
            codex: codex
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CODEX)),
        })
    }
}

fn next_non_empty_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<String, ConfigError> {
    let value = arguments.next().ok_or(ConfigError::MissingValue(option))?;
    if value.is_empty() {
        return Err(ConfigError::EmptyValue(option));
    }
    Ok(value)
}

fn set_once<T>(slot: &mut Option<T>, option: &'static str, value: T) -> Result<(), ConfigError> {
    if slot.replace(value).is_some() {
        return Err(ConfigError::DuplicateOption(option));
    }
    Ok(())
}

pub(crate) fn usage() -> &'static str {
    "Usage: host-daemon [--codex <PATH>] [--state-dir <PATH>] [--configure]\nOptional one-time configuration: --relay-url <URL> --relay-token <TOKEN> --runner-id <ID>\n\n--configure reads relayUrl, relayToken and runnerId as JSON from stdin and stores them in macOS Keychain. Normal startup restores that configuration.\nThe Host Daemon exposes owner-only host.sock in --state-dir (default ~/.bex) and connects outbound to the Phoenix relay as runner <ID>. --codex defaults to codex on PATH."
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    #[error("help requested")]
    Help,
    #[error("--relay-url is required")]
    MissingRelayUrl,
    #[error("--relay-token is required")]
    MissingRelayToken,
    #[error("--runner-id is required")]
    MissingRunnerId,
    #[error("{0} requires a value")]
    MissingValue(&'static str),
    #[error("{0} must not be empty")]
    EmptyValue(&'static str),
    #[error("{0} may be supplied only once")]
    DuplicateOption(&'static str),
    #[error("unknown argument {0}")]
    UnknownArgument(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<StartupConfig, ConfigError> {
        StartupConfig::parse_from(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn parses_required_relay_configuration_with_deterministic_codex_default() {
        assert_eq!(
            parse(&[
                "--relay-url",
                "wss://relay.example.test/socket/websocket",
                "--relay-token",
                "secret",
                "--runner-id",
                "runner-1",
            ])
            .unwrap(),
            StartupConfig {
                relay: Some(host_protocol::RelayEndpoint { relay_url: "wss://relay.example.test/socket/websocket".to_owned(), relay_token: "secret".to_owned(), runner_id: "runner-1".to_owned() }),
                configure: false,
                codex: PathBuf::from(DEFAULT_CODEX),
                state_dir: None,
            }
        );
    }

    #[test]
    fn parses_the_explicit_codex_override() {
        assert_eq!(
            parse(&[
                "--relay-url",
                "ws://127.0.0.1:4000/socket/websocket",
                "--relay-token",
                "relay-token",
                "--runner-id",
                "host-a",
                "--codex",
                "/opt/bin/codex",
            ])
            .unwrap()
            .codex,
            PathBuf::from("/opt/bin/codex")
        );
    }

    #[test]
    fn rejects_missing_duplicate_empty_and_ssh_arguments() {
        assert!(parse(&[]).unwrap().relay.is_none());
        assert_eq!(
            parse(&["--relay-url", "ws://relay"]),
            Err(ConfigError::MissingRelayToken)
        );
        assert_eq!(
            parse(&["--relay-url", "ws://relay", "--relay-token", "token",]),
            Err(ConfigError::MissingRunnerId)
        );
        assert_eq!(
            parse(&["--relay-url"]),
            Err(ConfigError::MissingValue("--relay-url"))
        );
        assert_eq!(
            parse(&["--relay-url", ""]),
            Err(ConfigError::EmptyValue("--relay-url"))
        );
        assert_eq!(
            parse(&["--relay-url", "ws://relay", "--relay-url", "ws://other",]),
            Err(ConfigError::DuplicateOption("--relay-url"))
        );
        assert_eq!(
            parse(&[
                "--relay-url",
                "ws://relay",
                "--relay-token",
                "token",
                "--runner-id",
                "runner",
                "--listen",
                "127.0.0.1:49152",
            ]),
            Err(ConfigError::UnknownArgument("--listen".to_owned()))
        );
        assert_eq!(
            parse(&[
                "--relay-url",
                "ws://relay",
                "--relay-token",
                "token",
                "--runner-id",
                "runner",
                "--settings",
                "state.json",
            ]),
            Err(ConfigError::UnknownArgument("--settings".to_owned()))
        );
    }

    #[test]
    fn usage_describes_only_outbound_relay_options() {
        let help = usage();
        assert!(help.contains("--relay-url <URL>"));
        assert!(help.contains("--relay-token <TOKEN>"));
        assert!(help.contains("--runner-id <ID>"));
        assert!(!help.contains("--listen"));
        assert!(!help.contains("--pair-address"));
    }
}
