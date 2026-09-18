//! Shared invocation completion and explicit provider inputs.
use crate::{client::Input, state::Snapshot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum InvocationKind {
    Plugin,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Invocation {
    pub kind: InvocationKind,
    pub name: String,
    pub path: String,
}
impl InvocationKind {
    fn sigil(self) -> char {
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
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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

/// Cursor and range are UTF-8 byte offsets. Reject mail addresses, paths and URLs.
fn query(text: &str, cursor: usize) -> Option<(std::ops::Range<usize>, InvocationKind, &str)> {
    let before = text.get(..cursor)?;
    let start = before
        .rfind(char::is_whitespace)
        .map_or(0, |i| i + before[i..].chars().next().unwrap().len_utf8());
    let token = &before[start..];
    let trigger = token.chars().next()?;
    let kind = match trigger {
        '@' | '＠' => InvocationKind::Plugin,
        '/' | '／' => InvocationKind::Skill,
        _ => return None,
    };
    let filter = &token[trigger.len_utf8()..];
    if filter.contains(['/', '@', ':', '\\', '[', ']', '(', ')', '`']) {
        return None;
    }
    Some((start..cursor, kind, filter))
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerSuggestions {
    pub candidates: Vec<ComposerCandidate>,
    pub status: Option<String>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn composer_suggestions(&self, text: String, cursor: u32) -> Option<ComposerSuggestions> {
        let (_, kind, filter) = self.composer_query(&text, cursor as usize)?;
        let catalog = self
            .composer_catalog
            .as_ref()
            .filter(|c| c.cwd == self.navigation.cwd);
        let filter = filter.to_lowercase();
        let candidates: Vec<_> = catalog
            .into_iter()
            .flat_map(|c| &c.candidates)
            .filter(|c| {
                c.invocation.kind == kind
                    && (c.invocation.name.to_lowercase().contains(&filter)
                        || c.description.to_lowercase().contains(&filter))
            })
            .cloned()
            .collect();
        let status = match catalog {
            None => Some("候補を読み込み中…".into()),
            Some(c) if c.loading => Some("候補を読み込み中…".into()),
            Some(c) if !c.errors.is_empty() => Some(c.errors.join("\n")),
            Some(_) if candidates.is_empty() => Some("該当する候補がありません".into()),
            _ => None,
        };
        Some(ComposerSuggestions { candidates, status })
    }
}

impl Snapshot {
    pub(crate) fn composer_query<'a>(
        &self,
        text: &'a str,
        cursor: usize,
    ) -> Option<(std::ops::Range<usize>, InvocationKind, &'a str)> {
        if self
            .drafts
            .get(&self.navigation.draft_key)
            .and_then(|d| d.model.as_deref())
            .is_some_and(|m| m.starts_with("claude:"))
        {
            return None;
        }
        query(text, cursor)
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerInsertion {
    pub text: String,
    pub cursor: u32,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn insert_invocation(
    mut text: String,
    cursor: u32,
    kind: InvocationKind,
    name: String,
) -> Option<ComposerInsertion> {
    let (range, trigger, _) = query(&text, cursor as usize)?;
    if kind != trigger {
        return None;
    }
    let token = format!("{}{name}", kind.sigil());
    let cursor = range.start + token.len() + 1;
    text.replace_range(range, &format!("{token} "));
    Some(ComposerInsertion {
        text,
        cursor: cursor as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, Event, Intent, reduce};

    fn skill() -> Invocation {
        Invocation {
            kind: InvocationKind::Skill,
            name: "review".into(),
            path: "/project/.agents/skills/review/SKILL.md".into(),
        }
    }

    #[test]
    fn completion_preserves_unicode_prefix_and_suffix() {
        let text = "日本語 /rev 後半";
        let cursor = "日本語 /rev".len() as u32;
        let inserted = insert_invocation(text.into(), cursor, skill().kind, skill().name).unwrap();
        assert_eq!(inserted.text, "日本語 $review  後半");
        assert_eq!(inserted.cursor as usize, "日本語 $review ".len());
        assert_eq!(
            query("＠rev", "＠rev".len()).unwrap().1,
            InvocationKind::Plugin
        );
        assert_eq!(
            query("／rev", "／rev".len()).unwrap().1,
            InvocationKind::Skill
        );
        for text in [
            "a@b",
            "https://example.org/",
            "path/to/",
            " /tmp/file",
            "$review ",
        ] {
            assert!(query(text, text.len()).is_none(), "{text}");
        }
        assert!(query("日本語 /", 1).is_none());
    }

    #[test]
    fn removed_or_renamed_invocations_do_not_survive_text_edits() {
        let draft = Draft {
            text: "$review ".into(),
            invocations: vec![skill()],
            ..Default::default()
        };
        let snapshot = Snapshot {
            drafts: std::sync::Arc::new(
                [(String::from("chat"), std::sync::Arc::new(draft))].into(),
            ),
            ..Default::default()
        };
        for text in ["plain text", "$review-other", "prefix$review", ""] {
            let (next, _) = reduce(
                &snapshot,
                Event::Intent(Intent::SetDraftText {
                    thread_id: "chat".into(),
                    text: text.into(),
                }),
            );
            assert!(next.drafts["chat"].invocations.is_empty(), "{text}");
        }
        for text in ["$review", "$review 日本語", "$review。"] {
            assert!(skill().is_in(text));
        }
    }

    #[test]
    fn editing_loads_catalog_once_and_selection_preserves_settings() {
        let snapshot = Snapshot::default();
        let key = snapshot.navigation.draft_key.clone();
        let (snapshot, effects) = reduce(
            &snapshot,
            Event::Intent(Intent::EditComposer {
                thread_id: key.clone(),
                text: "/".into(),
                cursor: 1,
            }),
        );
        assert_eq!(effects.len(), 1);
        assert!(snapshot.composer_catalog.as_ref().unwrap().loading);
        let (snapshot, effects) = reduce(
            &snapshot,
            Event::Intent(Intent::EditComposer {
                thread_id: key.clone(),
                text: "/rev suffix".into(),
                cursor: 4,
            }),
        );
        assert!(effects.is_empty());
        let mut draft = snapshot.drafts[&key].as_ref().clone();
        draft.model = Some("selected-model".into());
        draft.effort = Some("high".into());
        let snapshot = reduce(
            &snapshot,
            Event::Intent(Intent::SetDraft {
                thread_id: key.clone(),
                draft,
            }),
        )
        .0;
        let snapshot = reduce(
            &snapshot,
            Event::Intent(Intent::InsertInvocation {
                thread_id: key.clone(),
                text: "$review suffix".into(),
                invocation: skill(),
            }),
        )
        .0;
        assert_eq!(
            snapshot.drafts[&key].model.as_deref(),
            Some("selected-model")
        );
        assert_eq!(snapshot.drafts[&key].effort.as_deref(), Some("high"));
        assert_eq!(snapshot.drafts[&key].invocations, vec![skill()]);
    }

    #[test]
    fn selected_paths_survive_wire_encoding_and_reselection() {
        let mut snapshot = Snapshot::default();
        let mut other = skill();
        other.path = "/other/review/SKILL.md".into();
        let plugin = Invocation {
            kind: InvocationKind::Plugin,
            name: "Example Plugin".into(),
            path: "plugin://example@marketplace".into(),
        };
        for (text, invocation) in [
            ("/rev", skill()),
            ("$review /rev", other.clone()),
            ("$review @Ex", plugin.clone()),
        ] {
            let insertion = insert_invocation(
                text.into(),
                text.len() as u32,
                invocation.kind,
                invocation.name.clone(),
            )
            .unwrap();
            snapshot = reduce(
                &snapshot,
                Event::Intent(Intent::InsertInvocation {
                    thread_id: "chat".into(),
                    text: insertion.text,
                    invocation,
                }),
            )
            .0;
        }
        let draft = snapshot.drafts["chat"].as_ref();
        assert_eq!(draft.invocations, vec![other.clone(), plugin.clone()]);
        let restored: Draft =
            serde_json::from_str(&serde_json::to_string(&draft).unwrap()).unwrap();
        assert_eq!(&restored, draft);
        let inputs = restored
            .invocations
            .iter()
            .map(Invocation::input)
            .collect::<Vec<_>>();
        assert_eq!(
            Input::content(&inputs),
            serde_json::json!([
                {"type":"skill","name":"review","path":other.path},
                {"type":"mention","name":"Example Plugin","path":plugin.path}
            ])
        );
    }
}
