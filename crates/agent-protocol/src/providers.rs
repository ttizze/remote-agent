//! Configured instances own routing; open driver names identify implementations.
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidProviderIdentifier;
impl std::fmt::Display for InvalidProviderIdentifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "provider identifier must be a 1–64 character ASCII slug starting with a letter",
        )
    }
}
impl std::error::Error for InvalidProviderIdentifier {}

fn validate_slug(value: &str) -> Result<(), InvalidProviderIdentifier> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphabetic()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(InvalidProviderIdentifier);
    }
    Ok(())
}

macro_rules! slug {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = InvalidProviderIdentifier;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                validate_slug(&value)?;
                Ok(Self(value))
            }
        }
        impl FromStr for $name {
            type Err = InvalidProviderIdentifier;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.to_owned().try_into()
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}
slug!(ProviderDriver);
slug!(ProviderInstanceId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRef {
    pub instance_id: ProviderInstanceId,
    pub driver: ProviderDriver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderAvailability {
    Starting,
    Ready,
    Disabled,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstance {
    pub reference: ProviderRef,
    pub display_name: String,
    pub availability: ProviderAvailability,
    pub capabilities: crate::session::Capabilities,
    pub requires_account: bool,
}

/// Values are Host-owned; debug output never includes environment contents.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
    pub sensitive: bool,
    pub value_redacted: bool,
}
pub fn validate_environment(variables: &[EnvironmentVariable]) -> Result<(), &'static str> {
    let mut names = std::collections::HashSet::new();
    for variable in variables {
        let name = variable.name.as_bytes();
        if name.is_empty()
            || name.len() > 128
            || !(name[0].is_ascii_alphabetic() || name[0] == b'_')
            || !name
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return Err("environment variable name is invalid");
        }
        if !names.insert(&variable.name) {
            return Err("environment variable name is duplicated");
        }
        if variable.value.contains('\0') {
            return Err("environment variable contains a null byte");
        }
        if variable.value_redacted && (!variable.sensitive || !variable.value.is_empty()) {
            return Err("redacted environment variable must omit its value");
        }
    }
    Ok(())
}
impl std::fmt::Debug for EnvironmentVariable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentVariable")
            .field("name", &self.name)
            .field("sensitive", &self.sensitive)
            .field("value_redacted", &self.value_redacted)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConfig {
    pub driver: ProviderDriver,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub environment: Vec<EnvironmentVariable>,
    pub enabled: bool,
    #[serde(with = "crate::protocol::json")]
    pub config: serde_json::Value,
}
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("driver", &self.driver)
            .field("display_name", &self.display_name)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettings {
    pub revision: u64,
    pub instances: Vec<ConfiguredProvider>,
}
impl ProviderSettings {
    /// A client can edit a saved secret by returning the redacted sentinel.
    pub fn redacted(mut self) -> Self {
        for instance in &mut self.instances {
            for variable in &mut instance.config.environment {
                if variable.sensitive {
                    variable.value.clear();
                    variable.value_redacted = true;
                }
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredProvider {
    pub instance_id: ProviderInstanceId,
    pub config: ProviderConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderMutation {
    Create {
        instance_id: ProviderInstanceId,
        config: ProviderConfig,
    },
    Upsert {
        instance_id: ProviderInstanceId,
        config: ProviderConfig,
    },
    Remove {
        instance_id: ProviderInstanceId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadProviderSettings {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderInstance {
    pub operation_id: uuid::Uuid,
    pub revision: u64,
    pub mutation: ProviderMutation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn environment_names_values_and_redacted_sentinels_are_validated_independently() {
        let variable = EnvironmentVariable {
            name: "A0_B".into(),
            value: "example".into(),
            sensitive: false,
            value_redacted: false,
        };
        assert_eq!(validate_environment(&[]), Ok(()));
        for name in ["_".to_owned(), "a".repeat(128)] {
            assert_eq!(
                validate_environment(&[EnvironmentVariable {
                    name,
                    ..variable.clone()
                }]),
                Ok(())
            );
        }
        for name in [
            "".to_owned(),
            "a".repeat(129),
            "1A".into(),
            "A-B".into(),
            "A B".into(),
            "A\0".into(),
            "é".into(),
        ] {
            assert_eq!(
                validate_environment(&[EnvironmentVariable {
                    name,
                    ..variable.clone()
                }]),
                Err("environment variable name is invalid")
            );
        }
        assert_eq!(
            validate_environment(&[variable.clone(), variable.clone()]),
            Err("environment variable name is duplicated")
        );
        assert_eq!(
            validate_environment(&[
                variable.clone(),
                EnvironmentVariable {
                    name: "a0_b".into(),
                    ..variable.clone()
                }
            ]),
            Ok(())
        );
        assert_eq!(
            validate_environment(&[EnvironmentVariable {
                value: "a\0b".into(),
                ..variable.clone()
            }]),
            Err("environment variable contains a null byte")
        );
        for sensitive in [false, true] {
            for value_redacted in [false, true] {
                for value in ["", "example"] {
                    let result = validate_environment(&[EnvironmentVariable {
                        sensitive,
                        value_redacted,
                        value: value.into(),
                        ..variable.clone()
                    }]);
                    assert_eq!(
                        result.is_ok(),
                        !value_redacted || (sensitive && value.is_empty())
                    );
                }
            }
        }
    }

    #[test]
    fn open_driver_names_and_distinct_instances_round_trip_without_a_builtin_registry() {
        let reference = ProviderRef {
            instance_id: "personal_2".parse().unwrap(),
            driver: "ForkDriver-unknown".parse().unwrap(),
        };
        assert_eq!(
            serde_json::from_slice::<ProviderRef>(&serde_json::to_vec(&reference).unwrap())
                .unwrap(),
            reference
        );
        assert_eq!(
            crate::protocol::decode::<ProviderRef>(&crate::protocol::encode(&reference).unwrap())
                .unwrap(),
            reference
        );
        for invalid in [
            "", "_first", "1first", " white", "white ", "a/b", "a.b", "é",
        ] {
            assert!(invalid.parse::<ProviderDriver>().is_err());
            assert!(invalid.parse::<ProviderInstanceId>().is_err());
        }
        assert!("a".repeat(64).parse::<ProviderInstanceId>().is_ok());
        assert!("a".repeat(65).parse::<ProviderInstanceId>().is_err());
    }

    #[test]
    fn settings_frames_preserve_unknown_driver_payloads_and_redact_only_declared_sensitive_values()
    {
        let variable = EnvironmentVariable {
            name: "EXAMPLE_VALUE".into(),
            value: "placeholder".into(),
            sensitive: true,
            value_redacted: false,
        };
        let settings = ProviderSettings {
            revision: 7,
            instances: vec![ConfiguredProvider {
                instance_id: "work".parse().unwrap(),
                config: ProviderConfig {
                    driver: "FutureDriver".parse().unwrap(),
                    display_name: Some("Experimental".into()),
                    accent_color: None,
                    environment: vec![
                        variable.clone(),
                        EnvironmentVariable {
                            sensitive: false,
                            name: "PUBLIC_VALUE".into(),
                            ..variable.clone()
                        },
                    ],
                    enabled: false,
                    config: serde_json::json!({"unknown":[true, null, {"large":1234567890}]}),
                },
            }],
        };
        let decoded: ProviderSettings =
            crate::protocol::decode(&crate::protocol::encode(&settings).unwrap()).unwrap();
        assert_eq!(decoded, settings);
        assert!(!format!("{settings:?}").contains("placeholder"));
        let redacted = settings.clone().redacted();
        assert!(redacted.instances[0].config.environment[0].value.is_empty());
        assert!(redacted.instances[0].config.environment[0].value_redacted);
        assert_eq!(
            redacted.instances[0].config.environment[1].value,
            "placeholder"
        );
        assert!(!redacted.instances[0].config.environment[1].value_redacted);
        assert_eq!(
            redacted.instances[0].config.config,
            settings.instances[0].config.config
        );
        assert_eq!(
            settings.instances[0].config.environment[0].value,
            "placeholder"
        );
    }

    proptest! {
        #[test]
        fn environment_names_preserve_ascii_case_and_accept_non_null_values(
            name in "[A-Za-z_][A-Za-z0-9_]{0,127}",
            value in "[^\u{0000}]{0,256}",
            sensitive in any::<bool>(),
        ) {
            prop_assert_eq!(validate_environment(&[EnvironmentVariable {
                name, value, sensitive, value_redacted: false,
            }]), Ok(()));
        }

        #[test]
        fn valid_open_names_keep_every_byte(value in "[A-Za-z][A-Za-z0-9_-]{0,63}") {
            let driver = value.parse::<ProviderDriver>().unwrap();
            let instance = value.parse::<ProviderInstanceId>().unwrap();
            prop_assert_eq!(driver.as_str(), value.as_str());
            prop_assert_eq!(instance.as_str(), value.as_str());
            prop_assert_eq!(serde_json::from_value::<ProviderDriver>(serde_json::json!(value.clone())).unwrap(), driver);
        }

        #[test]
        fn non_letter_prefixes_are_rejected(prefix in "[0-9_-]", tail in "[A-Za-z0-9_-]{0,62}") {
            let value = format!("{prefix}{tail}");
            prop_assert!(value.parse::<ProviderDriver>().is_err());
            prop_assert!(value.parse::<ProviderInstanceId>().is_err());
        }
    }
}
