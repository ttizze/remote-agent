use crate::state::Snapshot;
use agent_protocol::composer::*;

pub fn candidate_label(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn should_refresh_catalog(
    loading: Option<bool>,
    filter: &str,
    text_changed: bool,
) -> bool {
    loading.is_none_or(|loading| !loading && filter.is_empty() && text_changed)
}
/// Cursor and range are UTF-8 byte offsets. Reject mail addresses, paths and URLs.
pub(crate) fn query(
    text: &str,
    cursor: usize,
) -> Option<(std::ops::Range<usize>, InvocationKind, &str)> {
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
        let (_, kind, filter) = query(&text, cursor as usize)?;
        let catalog = self
            .composer_catalog
            .as_ref()
            .filter(|c| c.cwd == self.navigation.cwd);
        let filter = filter.to_lowercase();
        let provider = self.provider_selection(self.composer_key().clone(), None);
        let candidates: Vec<_> = catalog
            .into_iter()
            .flat_map(|c| &c.candidates)
            .filter(|c| {
                Some(&c.invocation.instance_id) == provider.as_ref()
                    && c.invocation.kind == kind
                    && (c.invocation.name.to_lowercase().contains(&filter)
                        || c.description.to_lowercase().contains(&filter))
            })
            .cloned()
            .collect();
        let errors =
            catalog.and_then(|catalog| provider.as_ref().and_then(|id| catalog.errors.get(id)));
        let status = match catalog {
            None => Some("候補を読み込み中…".into()),
            Some(c) if c.loading && candidates.is_empty() => Some("候補を読み込み中…".into()),
            Some(_) if errors.is_some_and(|errors| !errors.is_empty()) => {
                errors.map(|errors| errors.join("\n"))
            }
            Some(_) if candidates.is_empty() => Some("該当する候補がありません".into()),
            _ => None,
        };
        Some(ComposerSuggestions { candidates, status })
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
    use agent_protocol::operations::Input;

    fn skill() -> Invocation {
        Invocation {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            kind: InvocationKind::Skill,
            name: "review".into(),
            path: "/project/.agents/skills/review/SKILL.md".into(),
        }
    }

    #[rstest::rstest]
    #[case("Review code", "Review code")]
    #[case("  First line\nSecond line\r\n", "First line Second line")]
    #[case("Review\t\u{2028}code", "Review code")]
    #[case("\n\t  ", "")]
    fn candidate_labels_keep_words_on_one_line(#[case] text: &str, #[case] expected: &str) {
        assert_eq!(candidate_label(text), expected);
    }

    #[rstest::rstest]
    #[case::missing(None, "rev", false, true)]
    #[case::prefetch(Some(true), "", true, false)]
    #[case::refresh(Some(false), "", true, true)]
    #[case::same_text(Some(false), "", false, false)]
    #[case::filtering(Some(false), "rev", true, false)]
    fn catalog_refreshes_only_when_needed(
        #[case] loading: Option<bool>,
        #[case] filter: &str,
        #[case] text_changed: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(
            should_refresh_catalog(loading, filter, text_changed),
            expected
        );
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
                [(
                    crate::state::DraftKey::from("chat"),
                    std::sync::Arc::new(draft),
                )]
                .into(),
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
            assert!(
                next.drafts[&crate::state::DraftKey::from("chat")]
                    .invocations
                    .is_empty(),
                "{text}"
            );
        }
        for text in ["$review", "$review 日本語", "$review。"] {
            assert!(skill().is_in(text));
        }
        assert_eq!(
            skill().replace_in(
                "日本語 $review。 $review-other prefix$review $review",
                "/review"
            ),
            "日本語 /review。 $review-other prefix$review /review"
        );
    }

    #[test]
    fn catalog_candidates_and_failures_belong_to_the_selected_provider() {
        let codex = skill();
        let mut claude = codex.clone();
        claude.instance_id = "claude"
            .parse::<crate::session::ProviderInstanceId>()
            .unwrap();
        let mut snapshot = Snapshot {
            provider_instances: crate::test_support::instances(),
            composer_catalog: Some(std::sync::Arc::new(ComposerCatalog {
                candidates: [codex.clone(), claude.clone()]
                    .map(|invocation| ComposerCandidate {
                        invocation,
                        description: String::new(),
                    })
                    .into(),
                errors: [(
                    "claude"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    vec!["Claude unavailable".into()],
                )]
                .into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let suggestions = snapshot.composer_suggestions("/".into(), 1).unwrap();
        assert_eq!(suggestions.candidates[0].invocation, codex);
        assert_eq!(suggestions.candidates.len(), 1);
        assert!(suggestions.status.is_none());
        std::sync::Arc::make_mut(
            std::sync::Arc::make_mut(&mut snapshot.drafts)
                .entry(snapshot.navigation.draft_key.clone())
                .or_default(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            instance_id: "claude"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "model".into(),
        });
        let suggestions = snapshot.composer_suggestions("/".into(), 1).unwrap();
        assert_eq!(suggestions.candidates[0].invocation, claude);
        assert_eq!(suggestions.candidates.len(), 1);
        assert_eq!(suggestions.status.as_deref(), Some("Claude unavailable"));
    }

    #[test]
    fn editing_loads_catalog_once_and_selection_preserves_settings() {
        let snapshot = Snapshot {
            connected: true,
            ..Default::default()
        };
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
        draft.model = Some(agent_protocol::models::ModelRef {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "selected-model".into(),
        });
        draft.options = agent_protocol::models::with_model_option(
            &draft.options,
            "reasoningEffort",
            Some(agent_protocol::models::ModelOptionValue::String(
                "high".into(),
            )),
        );
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
            snapshot.drafts[&key].model.as_ref(),
            Some(&agent_protocol::models::ModelRef {
                instance_id: "codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                id: "selected-model".into()
            })
        );
        assert_eq!(
            agent_protocol::models::model_option_string(
                &snapshot.drafts[&key].options,
                "reasoningEffort"
            ),
            Some("high")
        );
        assert_eq!(snapshot.drafts[&key].invocations, vec![skill()]);
    }

    #[test]
    fn prefetch_follows_connection_directory_and_provider() {
        let (offline, _) = reduce(
            &Snapshot::default(),
            Event::Intent(Intent::NewChat {
                cwd: "/project".into(),
            }),
        );
        assert!(offline.composer_catalog.is_none());
        let (connected, _) = reduce(&offline, Event::Connected);
        let catalog = connected.composer_catalog.as_ref().unwrap();
        assert_eq!(catalog.cwd, "/project");
        assert!(catalog.loading);
        let (edited, effects) = reduce(
            &connected,
            Event::Intent(Intent::EditComposer {
                thread_id: connected.navigation.draft_key.clone(),
                text: "/".into(),
                cursor: 1,
            }),
        );
        assert!(
            effects.is_empty(),
            "typing must share the prefetch already in flight"
        );
        assert!(std::sync::Arc::ptr_eq(
            catalog,
            edited.composer_catalog.as_ref().unwrap()
        ));
        let (other, _) = reduce(
            &edited,
            Event::Intent(Intent::NewChat {
                cwd: "/other".into(),
            }),
        );
        assert_eq!(other.composer_catalog.as_ref().unwrap().cwd, "/other");
        assert!(
            other
                .composer_catalog
                .as_ref()
                .unwrap()
                .candidates
                .is_empty()
        );
        let (disconnected, _) = reduce(&other, Event::Disconnected("offline".into()));
        assert!(disconnected.composer_catalog.is_none());
        let (reconnected, _) = reduce(&disconnected, Event::Connected);
        assert_eq!(reconnected.composer_catalog.as_ref().unwrap().cwd, "/other");
        let mut claude = offline;
        std::sync::Arc::make_mut(
            std::sync::Arc::make_mut(&mut claude.drafts)
                .get_mut(&claude.navigation.draft_key)
                .unwrap(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            instance_id: "claude"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "claude".into(),
        });
        let (claude, _) = reduce(&claude, Event::Connected);
        assert!(claude.composer_catalog.is_some());
        assert!(claude.composer_suggestions("/".into(), 1).is_some());
    }

    proptest::proptest! {
        #[test]
        fn refresh_keeps_matching_candidates_without_a_loading_message(filter in "[a-z]{0,12}") {
            use crate::state::operations::Operation;
            let mut snapshot = Snapshot {
                provider_instances: crate::test_support::instances(),
            composer_catalog: Some(std::sync::Arc::new(ComposerCatalog {
                    candidates: vec![ComposerCandidate { invocation: skill(), description: "Review code".into() }],
                    ..Default::default()
                })),
                ..Default::default()
            };
            crate::state::operations::LoadComposerCatalog { cwd: String::new() }.prepare(&mut snapshot).unwrap();
            let text = format!("/{filter}");
            let suggestions = snapshot.composer_suggestions(text.clone(), text.len() as u32).unwrap();
            let matches = "review".contains(&filter) || "review code".contains(&filter);
            proptest::prop_assert_eq!(suggestions.candidates.len(), usize::from(matches));
            proptest::prop_assert_eq!(suggestions.status.as_deref(), if matches { None } else { Some("候補を読み込み中…") });
            proptest::prop_assert_eq!(&snapshot.composer_catalog.as_ref().unwrap().candidates[0].invocation, &skill());
        }
    }

    #[test]
    fn selected_paths_survive_wire_encoding_and_reselection() {
        let mut snapshot = Snapshot::default();
        let mut other = skill();
        other.path = "/other/review/SKILL.md".into();
        let plugin = Invocation {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
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
        let draft = snapshot.drafts[&crate::state::DraftKey::from("chat")].as_ref();
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
            inputs,
            vec![
                Input::Skill {
                    name: "review".into(),
                    path: other.path
                },
                Input::Mention {
                    name: "Example Plugin".into(),
                    path: plugin.path
                },
            ]
        );
    }
}
