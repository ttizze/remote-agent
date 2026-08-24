use std::{net::SocketAddr, path::PathBuf};

const DEFAULT_LISTEN: &str = "0.0.0.0:49152";
const DEFAULT_CODEX: &str = "codex";
const DEFAULT_KEYCHAIN_SERVICE: &str = "bex.host-daemon";
const DEFAULT_KEYCHAIN_ACCOUNT: &str = "host-identity";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupConfig {
    pub(crate) listen: SocketAddr,
    pub(crate) settings: PathBuf,
    pub(crate) codex: PathBuf,
    pub(crate) keychain_service: String,
    pub(crate) keychain_account: String,
    pub(crate) pair_addresses: Vec<String>,
}

impl StartupConfig {
    pub(crate) fn parse_from(
        arguments: impl IntoIterator<Item = String>,
    ) -> Result<Self, ConfigError> {
        let mut listen = None;
        let mut settings = None;
        let mut codex = None;
        let mut keychain_service = None;
        let mut keychain_account = None;
        let mut pair_addresses = Vec::new();
        let mut arguments = arguments.into_iter();

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--help" | "-h" => return Err(ConfigError::Help),
                "--listen" => set_once(
                    &mut listen,
                    "--listen",
                    next_value(&mut arguments, "--listen")?
                        .parse()
                        .map_err(|_| ConfigError::InvalidListen)?,
                )?,
                "--settings" => set_once(
                    &mut settings,
                    "--settings",
                    PathBuf::from(next_value(&mut arguments, "--settings")?),
                )?,
                "--codex" => set_once(
                    &mut codex,
                    "--codex",
                    PathBuf::from(next_value(&mut arguments, "--codex")?),
                )?,
                "--keychain-service" => set_once(
                    &mut keychain_service,
                    "--keychain-service",
                    next_value(&mut arguments, "--keychain-service")?,
                )?,
                "--keychain-account" => set_once(
                    &mut keychain_account,
                    "--keychain-account",
                    next_value(&mut arguments, "--keychain-account")?,
                )?,
                "--pair-address" => {
                    pair_addresses.push(next_value(&mut arguments, "--pair-address")?);
                }
                _ => return Err(ConfigError::UnknownArgument(argument)),
            }
        }

        Ok(Self {
            listen: listen.unwrap_or_else(|| {
                DEFAULT_LISTEN
                    .parse()
                    .expect("valid default listen address")
            }),
            settings: settings.ok_or(ConfigError::MissingSettings)?,
            codex: codex.unwrap_or_else(|| PathBuf::from(DEFAULT_CODEX)),
            keychain_service: keychain_service
                .unwrap_or_else(|| DEFAULT_KEYCHAIN_SERVICE.to_owned()),
            keychain_account: keychain_account
                .unwrap_or_else(|| DEFAULT_KEYCHAIN_ACCOUNT.to_owned()),
            pair_addresses,
        })
    }
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<String, ConfigError> {
    arguments.next().ok_or(ConfigError::MissingValue(option))
}

fn set_once<T>(slot: &mut Option<T>, option: &'static str, value: T) -> Result<(), ConfigError> {
    if slot.replace(value).is_some() {
        return Err(ConfigError::DuplicateOption(option));
    }
    Ok(())
}

pub(crate) fn usage() -> &'static str {
    "Usage: host-daemon --settings <PATH> [--listen <ADDR>] [--codex <PATH>] [--keychain-service <NAME>] [--keychain-account <NAME>] [--pair-address <ADDR>]...\n\n--listen defaults to 0.0.0.0:49152; on macOS, --codex defaults to the ChatGPT Desktop bundled Codex when available and otherwise codex on PATH; on other platforms it defaults to codex on PATH. Repeat --pair-address to include multiple connection addresses."
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    #[error("help requested")]
    Help,
    #[error("--settings is required")]
    MissingSettings,
    #[error("{0} requires a value")]
    MissingValue(&'static str),
    #[error("{0} may be supplied only once")]
    DuplicateOption(&'static str),
    #[error("--listen must be a socket address")]
    InvalidListen,
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
    fn parses_required_settings_with_deterministic_defaults() {
        assert_eq!(
            parse(&["--settings", "/tmp/bex/settings.json"]).unwrap(),
            StartupConfig {
                listen: DEFAULT_LISTEN.parse().unwrap(),
                settings: PathBuf::from("/tmp/bex/settings.json"),
                codex: PathBuf::from(DEFAULT_CODEX),
                keychain_service: DEFAULT_KEYCHAIN_SERVICE.to_owned(),
                keychain_account: DEFAULT_KEYCHAIN_ACCOUNT.to_owned(),
                pair_addresses: Vec::new(),
            }
        );
    }

    #[test]
    fn parses_every_supported_override() {
        assert_eq!(
            parse(&[
                "--listen",
                "127.0.0.1:1234",
                "--settings",
                "state.json",
                "--codex",
                "/opt/bin/codex",
                "--keychain-service",
                "example.bex",
                "--keychain-account",
                "identity-a",
                "--pair-address",
                "192.168.0.211:49152",
                "--pair-address",
                "[fd00::211]:49152",
            ])
            .unwrap(),
            StartupConfig {
                listen: "127.0.0.1:1234".parse().unwrap(),
                settings: PathBuf::from("state.json"),
                codex: PathBuf::from("/opt/bin/codex"),
                keychain_service: "example.bex".to_owned(),
                keychain_account: "identity-a".to_owned(),
                pair_addresses: vec![
                    "192.168.0.211:49152".to_owned(),
                    "[fd00::211]:49152".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn rejects_ambiguous_or_incomplete_arguments() {
        assert_eq!(parse(&[]), Err(ConfigError::MissingSettings));
        assert_eq!(
            parse(&["--settings"]),
            Err(ConfigError::MissingValue("--settings"))
        );
        assert_eq!(
            parse(&["--settings", "one", "--settings", "two"]),
            Err(ConfigError::DuplicateOption("--settings"))
        );
        assert_eq!(
            parse(&["--settings", "one", "--listen", "not-an-address"]),
            Err(ConfigError::InvalidListen)
        );
        assert_eq!(
            parse(&["--settings", "one", "--pair-address"]),
            Err(ConfigError::MissingValue("--pair-address"))
        );
        assert_eq!(
            parse(&["--settings", "one", "--pair"]),
            Err(ConfigError::UnknownArgument("--pair".to_owned()))
        );
    }

    #[test]
    fn usage_describes_codex_bundle_and_path_fallback() {
        let help = usage();
        assert!(help.contains("ChatGPT Desktop bundled Codex"));
        assert!(help.contains("codex on PATH"));
        assert!(help.contains("--codex <PATH>"));
    }
}
