//! The options a model offers in the composer, and the value each resolves to
//! for a selection.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub options: Vec<OptionChoice>,
    pub current_value: Option<String>,
    /// Values sent in the prompt rather than as the option (`ultrathink`).
    pub prompt_injected_values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BooleanOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OptionDescriptor {
    Select(SelectOption),
    Boolean(BooleanOption),
}

impl OptionDescriptor {
    pub fn id(&self) -> &str {
        match self {
            Self::Select(select) => &select.id,
            Self::Boolean(boolean) => &boolean.id,
        }
    }
}

impl SelectOption {
    fn fallback(&self) -> Option<String> {
        self.current_value.clone().or_else(|| {
            self.options
                .iter()
                .find(|option| option.is_default)
                .map(|option| option.id.clone())
        })
    }

    /// The choice a raw value selects: a listed choice, otherwise the
    /// descriptor's current or default choice. A prompt-injected value selects
    /// the default, since it travels in the prompt.
    pub fn resolve(&self, raw: Option<&str>) -> Option<String> {
        let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return self.fallback();
        };
        if self.options.is_empty() {
            return Some(raw.to_owned());
        }
        let listed = self.options.iter().any(|option| option.id == raw);
        if listed && self.prompt_injected_values.iter().any(|value| value == raw) {
            return self
                .options
                .iter()
                .find(|option| option.is_default)
                .map(|option| option.id.clone());
        }
        if listed {
            return Some(raw.to_owned());
        }
        self.fallback()
    }
}

/// The value of option `id` under a selection's options; `None` when the model
/// does not offer it or nothing applies.
pub fn option_value(
    descriptors: &[OptionDescriptor],
    selected: &BTreeMap<String, String>,
    id: &str,
) -> Option<String> {
    let raw = selected.get(id).map(String::as_str);
    match descriptors
        .iter()
        .find(|descriptor| descriptor.id() == id)?
    {
        OptionDescriptor::Select(select) => select.resolve(raw),
        OptionDescriptor::Boolean(boolean) => match raw {
            Some("true") => Some("true".into()),
            Some("false") => Some("false".into()),
            _ => boolean.current_value.map(|value| value.to_string()),
        },
    }
}

/// The selected value when a select option sends it in the prompt.
pub fn prompt_injected_value(
    descriptors: &[OptionDescriptor],
    raw: Option<&str>,
) -> Option<String> {
    let raw = raw.map(str::trim).filter(|raw| !raw.is_empty())?;
    descriptors
        .iter()
        .any(|descriptor| match descriptor {
            OptionDescriptor::Select(select) => select
                .prompt_injected_values
                .iter()
                .any(|value| value == raw),
            OptionDescriptor::Boolean(_) => false,
        })
        .then(|| raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, default: bool) -> OptionChoice {
        OptionChoice {
            id: id.into(),
            label: id.into(),
            description: None,
            is_default: default,
        }
    }

    fn effort() -> OptionDescriptor {
        OptionDescriptor::Select(SelectOption {
            id: "effort".into(),
            label: "Reasoning".into(),
            description: None,
            options: vec![
                choice("low", false),
                choice("high", true),
                choice("ultrathink", false),
            ],
            current_value: None,
            prompt_injected_values: vec!["ultrathink".into()],
        })
    }

    // shared/model.test.ts "descriptor helpers": selections resolve against the
    // listed choices and fall back to the default.
    #[test]
    fn selections_resolve_to_listed_choices_or_the_default() {
        let descriptors = [
            effort(),
            OptionDescriptor::Boolean(BooleanOption {
                id: "fastMode".into(),
                label: "Fast Mode".into(),
                description: None,
                current_value: None,
            }),
        ];
        let selected = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>()
        };
        let value =
            |pairs: &[(&str, &str)], id: &str| option_value(&descriptors, &selected(pairs), id);
        assert_eq!(value(&[], "effort").as_deref(), Some("high"));
        assert_eq!(
            value(&[("effort", "low")], "effort").as_deref(),
            Some("low")
        );
        assert_eq!(
            value(&[("effort", "max")], "effort").as_deref(),
            Some("high")
        );
        assert_eq!(
            value(&[("effort", "ultrathink")], "effort").as_deref(),
            Some("high")
        );
        assert_eq!(value(&[], "fastMode"), None);
        assert_eq!(
            value(&[("fastMode", "true")], "fastMode").as_deref(),
            Some("true")
        );
        assert_eq!(value(&[("contextWindow", "1m")], "contextWindow"), None);
        assert_eq!(
            prompt_injected_value(&descriptors, Some("ultrathink")).as_deref(),
            Some("ultrathink")
        );
        assert_eq!(prompt_injected_value(&descriptors, Some("high")), None);
    }
}
