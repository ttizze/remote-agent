use crate::localization::UiLanguage;
use agent_protocol::composer::*;
pub mod context;
pub mod stash;
pub(crate) mod template;

/// Keep selected lines quoted and leave room for the user's question.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn append_selection_to_draft(draft: &str, selection: &str) -> String {
    let quote = selection
        .split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    if draft.is_empty() {
        format!("{quote}\n\n")
    } else {
        format!("{draft}\n\n{quote}\n\n")
    }
}

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

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerSuggestions {
    pub candidates: Vec<ComposerCandidate>,
    pub status: Option<String>,
}

pub(crate) fn suggestions(
    language: UiLanguage,
    catalog: Option<&agent_protocol::composer::ComposerCatalog>,
    provider: Option<agent_protocol::session::ProviderKind>,
    text: String,
    cursor: u32,
) -> Option<ComposerSuggestions> {
    let (_, kind, filter) = query(&text, cursor as usize)?;
    let filter = filter.to_lowercase();
    let candidates: Vec<_> = catalog
        .into_iter()
        .flat_map(|c| &c.candidates)
        .filter(|c| {
            Some(c.invocation.provider) == provider
                && c.invocation.kind == kind
                && (filter.is_empty()
                    || c.invocation.name.to_lowercase().contains(&filter)
                    || c.description.to_lowercase().contains(&filter))
        })
        .cloned()
        .collect();
    let status = match catalog {
        None => Some(language.text("候補を読み込み中…").into()),
        Some(c) if c.loading && candidates.is_empty() => {
            Some(language.text("候補を読み込み中…").into())
        }
        Some(c)
            if provider
                .and_then(|provider| c.errors.get(&provider))
                .is_some_and(|errors| !errors.is_empty()) =>
        {
            Some(c.errors[&provider.unwrap()].join("\n"))
        }
        Some(_) if candidates.is_empty() => Some(language.text("該当する候補がありません").into()),
        _ => None,
    };
    Some(ComposerSuggestions { candidates, status })
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

/// What a key does while the composer lists completions, as the desktop
/// client's: the arrows move the highlight, Return and Tab insert the
/// highlighted candidate and Escape closes the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKey {
    Highlight(usize),
    Insert(usize),
    Dismiss,
}

/// `key` (`up`, `down`, `return`, `tab`, `escape`) over `count` candidates with
/// `highlighted` highlighted; `None` leaves the key to the field.
pub fn completion_key(key: &str, highlighted: usize, count: usize) -> Option<CompletionKey> {
    let last = count.checked_sub(1)?;
    Some(match key {
        "up" => CompletionKey::Highlight(highlighted.min(last).saturating_sub(1)),
        "down" => CompletionKey::Highlight((highlighted + 1).min(last)),
        "return" | "tab" => CompletionKey::Insert(highlighted.min(last)),
        "escape" => CompletionKey::Dismiss,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, Event, Intent, Snapshot, reduce};

    #[rstest::rstest]
    #[case("down", 0, 3, Some(CompletionKey::Highlight(1)))]
    #[case("down", 2, 3, Some(CompletionKey::Highlight(2)))]
    #[case("up", 1, 3, Some(CompletionKey::Highlight(0)))]
    #[case("up", 0, 3, Some(CompletionKey::Highlight(0)))]
    #[case("up", 7, 3, Some(CompletionKey::Highlight(1)))]
    #[case("return", 1, 3, Some(CompletionKey::Insert(1)))]
    #[case("tab", 9, 3, Some(CompletionKey::Insert(2)))]
    #[case("escape", 0, 1, Some(CompletionKey::Dismiss))]
    #[case("left", 0, 3, None)]
    #[case("return", 0, 0, None)]
    #[case("escape", 0, 0, None)]
    fn completion_keys_stay_on_the_listed_candidates(
        #[case] key: &str,
        #[case] highlighted: usize,
        #[case] count: usize,
        #[case] expected: Option<CompletionKey>,
    ) {
        assert_eq!(completion_key(key, highlighted, count), expected);
    }
    use agent_protocol::operations::Input;

    #[rstest::rstest]
    #[case("", "Needle", "> Needle\n\n")]
    #[case(
        "Keep my draft",
        "first\nsecond",
        "Keep my draft\n\n> first\n> second\n\n"
    )]
    #[case("入力", "行🌱\n", "入力\n\n> 行🌱\n> \n\n")]
    #[case("line\n", "a\r\nb", "line\n\n\n> a\r\n> b\n\n")]
    #[case("", "", "> \n\n")]
    fn selections_preserve_existing_input_and_quote_every_selected_line(
        #[case] draft: &str,
        #[case] selection: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(append_selection_to_draft(draft, selection), expected);
    }

    fn skill() -> Invocation {
        Invocation {
            provider: agent_protocol::session::ProviderKind::Codex,
            kind: InvocationKind::Skill,
            name: "review".into(),
            path: "/project/.agents/skills/review/SKILL.md".into(),
        }
    }

    #[test]
    fn quoting_a_selection_preserves_the_target_draft_after_navigation_changes() {
        let draft = Draft {
            text: "$review 下書き".into(),
            attachments: vec![crate::state::Attachment {
                path: "/fixture/添付.txt".into(),
                name: "添付.txt".into(),
                is_image: false,
            }],
            invocations: vec![skill()],
            ..Default::default()
        };
        let (snapshot, _) = reduce(
            &Snapshot::default(),
            "",
            Event::Intent(Intent::SetDraft {
                thread_id: "origin".into(),
                draft: draft.clone(),
            }),
        );
        let (snapshot, _) = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::SetDraftText {
                thread_id: "other".into(),
                text: "別の下書き".into(),
            }),
        );
        let (snapshot, _) = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/other".into(),
            }),
        );
        let (next, effects) = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::QuoteSelection {
                storage_scope: String::new(),
                thread_id: "origin".into(),
                text: "日本語 👋\n引用".into(),
            }),
        );
        assert_eq!(
            next.input.drafts[&"origin".into()].text,
            "$review 下書き\n\n> 日本語 👋\n> 引用\n\n"
        );
        assert_eq!(
            next.input.drafts[&"origin".into()].invocations,
            draft.invocations
        );
        assert_eq!(
            next.input.drafts[&"origin".into()].attachments,
            draft.attachments
        );
        assert_eq!(
            next.input.drafts[&"other".into()],
            snapshot.input.drafts[&"other".into()]
        );
        assert_eq!(
            next.panes.primary().navigation,
            snapshot.panes.primary().navigation
        );
        assert!(effects.is_empty());
        let (stale, effects) = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::QuoteSelection {
                storage_scope: "previous-account".into(),
                thread_id: "origin".into(),
                text: "古い選択".into(),
            }),
        );
        assert_eq!(stale.input.drafts, snapshot.input.drafts);
        assert!(effects.is_empty());
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
            input: crate::state::Input {
                drafts: std::sync::Arc::new(
                    [(
                        crate::state::DraftKey::from("chat"),
                        std::sync::Arc::new(draft),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        for text in ["plain text", "$review-other", "prefix$review", ""] {
            let (next, _) = reduce(
                &snapshot,
                "",
                Event::Intent(Intent::SetDraftText {
                    thread_id: "chat".into(),
                    text: text.into(),
                }),
            );
            assert!(
                next.input.drafts[&crate::state::DraftKey::from("chat")]
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
        use agent_protocol::session::ProviderKind;
        let codex = skill();
        let mut claude = codex.clone();
        claude.provider = ProviderKind::Claude;
        let mut snapshot = Snapshot {
            catalog: crate::state::Catalog {
                composer_catalogs: std::sync::Arc::new(
                    (Some(std::sync::Arc::new(ComposerCatalog {
                        candidates: [codex.clone(), claude.clone()]
                            .map(|invocation| ComposerCandidate {
                                invocation,
                                description: String::new(),
                            })
                            .into(),
                        errors: [(ProviderKind::Claude, vec!["Claude unavailable".into()])].into(),
                        ..Default::default()
                    })))
                    .into_iter()
                    .map(|catalog| (catalog.cwd.clone(), catalog))
                    .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        std::sync::Arc::make_mut(
            std::sync::Arc::make_mut(&mut snapshot.input.drafts)
                .entry(snapshot.panes.primary().navigation.draft_key.clone())
                .or_default(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            provider: ProviderKind::Codex,
            id: "model".into(),
        });
        let suggestions = crate::presentation::view::Presentation::from_snapshot(
            std::sync::Arc::new(snapshot.clone()),
            "".into(),
        )
        .unwrap()
        .composer()
        .composer_suggestions("/".into(), 1)
        .unwrap();
        assert_eq!(suggestions.candidates[0].invocation, codex);
        assert_eq!(suggestions.candidates.len(), 1);
        assert!(suggestions.status.is_none());
        std::sync::Arc::make_mut(
            std::sync::Arc::make_mut(&mut snapshot.input.drafts)
                .entry(snapshot.panes.primary().navigation.draft_key.clone())
                .or_default(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            provider: ProviderKind::Claude,
            id: "model".into(),
        });
        let suggestions = crate::presentation::view::Presentation::from_snapshot(
            std::sync::Arc::new(snapshot.clone()),
            "".into(),
        )
        .unwrap()
        .composer()
        .composer_suggestions("/".into(), 1)
        .unwrap();
        assert_eq!(suggestions.candidates[0].invocation, claude);
        assert_eq!(suggestions.candidates.len(), 1);
        assert_eq!(suggestions.status.as_deref(), Some("Claude unavailable"));
    }

    #[test]
    fn suggestions_match_names_or_descriptions_with_unicode_and_keep_provider_and_kind() {
        use InvocationKind::{Plugin, Skill};
        use agent_protocol::session::ProviderKind::{Claude, Codex};
        use std::sync::Arc;
        let candidate = |name: &str, description: &str, provider, kind| ComposerCandidate {
            invocation: Invocation {
                name: name.into(),
                provider,
                kind,
                ..skill()
            },
            description: description.into(),
        };
        let candidates = vec![
            candidate("İSTANBUL", "Name only match", Codex, Skill),
            candidate("deploy", "Remote OPERATIONS", Codex, Skill),
            candidate("audit", "Nothing related", Codex, Skill),
            candidate("İSTANBUL", "Remote OPERATIONS", Claude, Skill),
            candidate("İSTANBUL", "Remote OPERATIONS", Codex, Plugin),
        ];
        let mut snapshot = Snapshot {
            catalog: crate::state::Catalog {
                composer_catalogs: std::sync::Arc::new(
                    (Some(Arc::new(ComposerCatalog {
                        candidates: candidates.clone(),
                        ..Default::default()
                    })))
                    .into_iter()
                    .map(|catalog| (catalog.cwd.clone(), catalog))
                    .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(snapshot.panes.primary().navigation.draft_key.clone())
                .or_default(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            provider: Codex,
            id: "model".into(),
        });
        for (text, expected) in [
            ("/", vec![0, 1, 2]),
            ("/i\u{307}stan", vec![0]),
            ("/OPERATIONS", vec![1]),
            ("/istanbul", vec![]),
            ("@", vec![4]),
        ] {
            let result = crate::presentation::view::Presentation::from_snapshot(
                std::sync::Arc::new(snapshot.clone()),
                "".into(),
            )
            .unwrap()
            .composer()
            .composer_suggestions(text.into(), text.len() as u32)
            .unwrap();
            assert_eq!(
                result.candidates,
                expected
                    .into_iter()
                    .map(|index| candidates[index].clone())
                    .collect::<Vec<_>>(),
                "{text}",
            );
            assert_eq!(
                result.status.as_deref(),
                result
                    .candidates
                    .is_empty()
                    .then_some("該当する候補がありません"),
            );
        }
        assert_eq!(
            snapshot
                .catalog
                .composer_catalogs
                .get(&snapshot.panes.primary().navigation.cwd)
                .unwrap()
                .candidates,
            candidates
        );
    }

    #[test]
    fn editing_loads_catalog_once_and_selection_preserves_settings() {
        let snapshot = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        let (snapshot, effects) = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::EditComposer {
                thread_id: key.clone(),
                text: "/".into(),
                cursor: 1,
            }),
        );
        assert_eq!(effects.len(), 1);
        assert!(
            snapshot
                .catalog
                .composer_catalogs
                .get(&snapshot.panes.primary().navigation.cwd)
                .unwrap()
                .loading
        );
        let (snapshot, effects) = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::EditComposer {
                thread_id: key.clone(),
                text: "/rev suffix".into(),
                cursor: 4,
            }),
        );
        assert!(effects.is_empty());
        let mut draft = snapshot.input.drafts[&key].as_ref().clone();
        draft.model = Some(agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "selected-model".into(),
        });
        draft.effort = Some("high".into());
        let snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::SetDraft {
                thread_id: key.clone(),
                draft,
            }),
        )
        .0;
        let snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::InsertInvocation {
                thread_id: key.clone(),
                text: "$review suffix".into(),
                invocation: skill(),
            }),
        )
        .0;
        assert_eq!(
            snapshot.input.drafts[&key].model.as_ref(),
            Some(&agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "selected-model".into()
            })
        );
        assert_eq!(snapshot.input.drafts[&key].effort.as_deref(), Some("high"));
        assert_eq!(snapshot.input.drafts[&key].invocations, vec![skill()]);
    }

    #[test]
    fn prefetch_follows_connection_directory_and_provider() {
        let (offline, _) = reduce(
            &Snapshot::default(),
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/project".into(),
            }),
        );
        assert!(
            offline
                .catalog
                .composer_catalogs
                .get(&offline.panes.primary().navigation.cwd)
                .is_none()
        );
        let (connected, _) = reduce(&offline, "", Event::Connected);
        let catalog = connected
            .catalog
            .composer_catalogs
            .get(&connected.panes.primary().navigation.cwd)
            .unwrap();
        assert_eq!(catalog.cwd, "/project");
        assert!(catalog.loading);
        let (edited, effects) = reduce(
            &connected,
            "",
            Event::Intent(Intent::EditComposer {
                thread_id: connected.panes.primary().navigation.draft_key.clone(),
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
            edited
                .catalog
                .composer_catalogs
                .get(&edited.panes.primary().navigation.cwd)
                .unwrap()
        ));
        let (other, _) = reduce(
            &edited,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/other".into(),
            }),
        );
        assert_eq!(
            other
                .catalog
                .composer_catalogs
                .get(&other.panes.primary().navigation.cwd)
                .unwrap()
                .cwd,
            "/other"
        );
        assert!(
            other
                .catalog
                .composer_catalogs
                .get(&other.panes.primary().navigation.cwd)
                .unwrap()
                .candidates
                .is_empty()
        );
        let (disconnected, _) = reduce(&other, "", Event::Disconnected);
        assert!(
            disconnected
                .catalog
                .composer_catalogs
                .get(&disconnected.panes.primary().navigation.cwd)
                .is_none()
        );
        let (reconnected, _) = reduce(&disconnected, "", Event::Connected);
        assert_eq!(
            reconnected
                .catalog
                .composer_catalogs
                .get(&reconnected.panes.primary().navigation.cwd)
                .unwrap()
                .cwd,
            "/other"
        );
        let mut claude = offline;
        std::sync::Arc::make_mut(
            std::sync::Arc::make_mut(&mut claude.input.drafts)
                .get_mut(&claude.panes.primary().navigation.draft_key)
                .unwrap(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Claude,
            id: "claude".into(),
        });
        let (claude, _) = reduce(&claude, "", Event::Connected);
        assert!(
            claude
                .catalog
                .composer_catalogs
                .get(&claude.panes.primary().navigation.cwd)
                .is_some()
        );
        assert!(
            crate::presentation::view::Presentation::from_snapshot(
                std::sync::Arc::new(claude.clone()),
                "".into()
            )
            .unwrap()
            .composer()
            .composer_suggestions("/".into(), 1)
            .is_some()
        );
    }

    proptest::proptest! {
        #[test]
        fn refresh_keeps_matching_candidates_without_a_loading_message(filter in "[a-z]{0,12}") {
            use crate::state::operations::Operation;
            let mut snapshot = Snapshot {
                catalog: crate::state::Catalog {
                    composer_catalogs: std::sync::Arc::new((Some(std::sync::Arc::new(ComposerCatalog {
                        candidates: vec![ComposerCandidate { invocation: skill(), description: "Review code".into() }],
                        ..Default::default()
                    }))).into_iter().map(|catalog| (catalog.cwd.clone(), catalog)).collect()),
                    ..Default::default()
                },
                ..Default::default()
            };
            std::sync::Arc::make_mut(&mut snapshot.input.drafts).insert(snapshot.panes.primary().navigation.draft_key.clone(), std::sync::Arc::new(crate::state::Draft {
                model: Some(agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Codex, id: "model".into() }),
                ..Default::default()
            }));
            crate::state::operations::LoadComposerCatalog { cwd: String::new() }.prepare(&mut snapshot, "").unwrap();
            let text = format!("/{filter}");
            let suggestions = crate::presentation::view::Presentation::from_snapshot(std::sync::Arc::new(snapshot.clone()), "".into()).unwrap().composer().composer_suggestions(text.clone(), text.len() as u32).unwrap();
            let matches = "review".contains(&filter) || "review code".contains(&filter);
            proptest::prop_assert_eq!(suggestions.candidates.len(), usize::from(matches));
            proptest::prop_assert_eq!(suggestions.status.as_deref(), if matches { None } else { Some("候補を読み込み中…") });
            proptest::prop_assert_eq!(&snapshot.catalog.composer_catalogs.get(&snapshot.panes.primary().navigation.cwd).unwrap().candidates[0].invocation, &skill());
        }
    }

    #[test]
    fn selected_paths_survive_wire_encoding_and_reselection() {
        let mut snapshot = Snapshot::default();
        let mut other = skill();
        other.path = "/other/review/SKILL.md".into();
        let plugin = Invocation {
            provider: agent_protocol::session::ProviderKind::Codex,
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
                "",
                Event::Intent(Intent::InsertInvocation {
                    thread_id: "chat".into(),
                    text: insertion.text,
                    invocation,
                }),
            )
            .0;
        }
        let draft = snapshot.input.drafts[&crate::state::DraftKey::from("chat")].as_ref();
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
