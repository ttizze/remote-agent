//! Credential references understood by the DSH provider routes.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyProvider {
    #[default]
    DeepSeek,
    OpenRouter,
    NanoGpt,
    Venice,
    OpenAI,
    Anthropic,
    Gemini,
    Groq,
    Mistral,
    Xai,
}

impl KeyProvider {
    pub fn model_label(self) -> &'static str {
        if self == Self::DeepSeek {
            "DeepSeek公式"
        } else {
            self.label()
        }
    }

    pub const ALL: [Self; 10] = [
        Self::DeepSeek,
        Self::OpenRouter,
        Self::NanoGpt,
        Self::Venice,
        Self::OpenAI,
        Self::Anthropic,
        Self::Gemini,
        Self::Groq,
        Self::Mistral,
        Self::Xai,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::NanoGpt => "NanoGPT",
            Self::Venice => "Venice AI",
            Self::OpenAI => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::Groq => "Groq",
            Self::Mistral => "Mistral",
            Self::Xai => "xAI",
        }
    }

    pub fn reference(self) -> &'static str {
        match self {
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::NanoGpt => "NANOGPT_API_KEY",
            Self::Venice => "VENICE_API_KEY",
            Self::OpenAI => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Gemini => "GEMINI_API_KEY",
            Self::Groq => "GROQ_API_KEY",
            Self::Mistral => "MISTRAL_API_KEY",
            Self::Xai => "XAI_API_KEY",
        }
    }

    pub fn route(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek-official",
            Self::OpenRouter => "openrouter",
            Self::NanoGpt => "nanogpt",
            Self::Venice => "venice",
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "google",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Xai => "xai",
        }
    }

    pub fn key_url(self) -> &'static str {
        match self {
            Self::DeepSeek => "https://platform.deepseek.com/api_keys",
            Self::OpenRouter => "https://openrouter.ai/settings/keys",
            Self::NanoGpt => "https://nano-gpt.com/api",
            Self::Venice => "https://venice.ai/settings/api",
            Self::OpenAI => "https://platform.openai.com/api-keys",
            Self::Anthropic => "https://console.anthropic.com/settings/keys",
            Self::Gemini => "https://aistudio.google.com/apikey",
            Self::Groq => "https://console.groq.com/keys",
            Self::Mistral => "https://console.mistral.ai/api-keys",
            Self::Xai => "https://console.x.ai/",
        }
    }
}

/// A private credential mutation. Keys are never returned in metadata or Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyUpdate {
    pub provider: KeyProvider,
    /// None removes only the selected provider's BEX-managed key.
    pub key: Option<String>,
}

impl std::fmt::Debug for KeyUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyUpdate")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialSource {
    Missing,
    Managed,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRoute {
    pub provider: KeyProvider,
    pub source: CredentialSource,
    pub needs_attention: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadCredentials {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credentials {
    pub routes: Vec<CredentialRoute>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_never_contains_credential_values() {
        let update = KeyUpdate {
            provider: KeyProvider::OpenRouter,
            key: Some("fixture-secret".into()),
        };
        let debug = format!("{update:?}");
        assert!(!debug.contains("fixture-secret"));
        assert!(debug.contains("OpenRouter"));
    }
}
