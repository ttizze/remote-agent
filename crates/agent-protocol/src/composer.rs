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
        text.match_indices(&self.token()).any(|(start, token)| {
            (start == 0 || text[..start].ends_with(char::is_whitespace))
                && text[start + token.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '.' | '。' | '、'))
        })
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
    pub errors: Vec<String>,
}
