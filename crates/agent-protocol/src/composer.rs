//! Shared invocation completion and explicit provider inputs.
use crate::operations::Input;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationKind {
    Plugin,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    #[serde(rename = "instanceId")]
    pub instance_id: crate::session::ProviderInstanceId,
    pub kind: InvocationKind,
    pub name: String,
    pub path: String,
}
impl InvocationKind {
    pub fn sigil(self) -> char {
        match self {
            Self::Plugin => '@',
            Self::Skill => '$',
        }
    }
}
impl Invocation {
    pub fn token(&self) -> String {
        format!("{}{}", self.kind.sigil(), self.name)
    }
    pub fn is_in(&self, text: &str) -> bool {
        text.match_indices(&self.token())
            .any(|(start, token)| token_boundary(text, start, start + token.len()))
    }
    pub fn replace_in(&self, text: &str, replacement: &str) -> String {
        let mut replaced = String::with_capacity(text.len());
        let mut copied = 0;
        for (start, token) in text.match_indices(&self.token()) {
            let end = start + token.len();
            if token_boundary(text, start, end) {
                replaced.push_str(&text[copied..start]);
                replaced.push_str(replacement);
                copied = end;
            }
        }
        replaced.push_str(&text[copied..]);
        replaced
    }
    pub fn input(&self) -> Input {
        match self.kind {
            InvocationKind::Plugin => Input::Mention {
                name: self.name.clone(),
                path: self.path.clone(),
            },
            InvocationKind::Skill => Input::Skill {
                name: self.name.clone(),
                path: self.path.clone(),
            },
        }
    }
}

fn token_boundary(text: &str, start: usize, end: usize) -> bool {
    (start == 0 || text[..start].ends_with(char::is_whitespace))
        && text[end..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '.' | '。' | '、'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposerCandidate {
    pub invocation: Invocation,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComposerCatalog {
    pub cwd: String,
    pub loading: bool,
    pub candidates: Vec<ComposerCandidate>,
    pub errors: std::collections::HashMap<crate::session::ProviderInstanceId, Vec<String>>,
}
