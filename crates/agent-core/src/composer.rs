use crate::localization::UiLanguage;
use agent_protocol::composer::*;
pub mod code;
pub mod completions;
pub mod context;
pub(crate) mod input;
#[cfg(test)]
mod lean_conformance;
pub mod snapshot;
pub mod stash;
pub(crate) mod template;

/// Keep selected lines quoted and leave room for the user's question.
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
static CURRENCY_PREFIX: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\p{Sc}+").expect("currency prefix grammar"));

pub(crate) struct CompletionQuery<'a> {
    range: std::ops::Range<usize>,
    kind: InvocationKind,
    pub(crate) filter: &'a str,
    currency: bool,
}

// T3's currency trigger splits on these four ASCII characters. Unicode space
// remains in the query and is normalized by search, not by moving its range.
fn currency_query(before: &str) -> Option<CompletionQuery<'_>> {
    static CURRENCY_AMOUNT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\.?[0-9][0-9,._]*(?:[kKmMbBtT]|[eE][0-9]+)?[.,;:!?)]*$")
            .expect("currency amount grammar")
    });
    let start = source_token_start(before);
    let token = &before[start..];
    if !CURRENCY_PREFIX.is_match(token) {
        return None;
    }
    let filter = &token[token.chars().next()?.len_utf8()..];
    if CURRENCY_AMOUNT.is_match(filter) {
        return None;
    }
    Some(CompletionQuery {
        range: start..before.len(),
        kind: InvocationKind::Skill,
        filter,
        currency: true,
    })
}

fn source_token_start(before: &str) -> usize {
    before.rfind([' ', '\n', '\t', '\r']).map_or(0, |i| i + 1)
}

/// UTF-8 byte offsets. Currency skills use T3's token grammar; existing
/// slash/plugin prefixes retain their Unicode word and punctuation boundaries.
pub(crate) fn query(text: &str, cursor: usize) -> Option<CompletionQuery<'_>> {
    let before = text.get(..cursor)?;
    if let Some(query) = currency_query(before) {
        return Some(query);
    }
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
    Some(CompletionQuery {
        range: start..cursor,
        kind,
        filter,
        currency: false,
    })
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerSuggestions {
    pub candidates: Vec<ComposerCandidate>,
    pub status: Option<String>,
    pub query_key: String,
}

/// Provider-scoped immutable input for a native editor's caret-local queries.
/// The editor owns its selection; Core owns filtering and status precedence.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CompletionCatalog {
    pub candidates: Vec<ComposerCandidate>,
    pub loading: bool,
    pub error: Option<String>,
    pub language: UiLanguage,
}

pub(crate) fn completion_catalog(
    language: UiLanguage,
    catalog: Option<&agent_protocol::composer::ComposerCatalog>,
    provider: Option<agent_protocol::session::ProviderKind>,
) -> CompletionCatalog {
    CompletionCatalog {
        candidates: catalog
            .into_iter()
            .flat_map(|c| &c.candidates)
            .filter(|c| Some(c.invocation.provider) == provider)
            .cloned()
            .collect(),
        loading: catalog.is_none_or(|c| c.loading),
        error: catalog
            .and_then(|c| provider.and_then(|p| c.errors.get(&p)))
            .filter(|errors| !errors.is_empty())
            .map(|errors| errors.join("\n")),
        language,
    }
}

impl CompletionCatalog {
    pub fn suggestions(&self, text: &str, cursor: u32) -> Option<ComposerSuggestions> {
        let query = query(text, cursor as usize)?;
        let query_key = format!(
            "{:?}:{}:{}",
            query.kind,
            query.currency,
            crate::text::trim(query.filter).to_lowercase()
        );
        let filter = crate::text::trim(query.filter);
        let filter = if query.currency {
            CURRENCY_PREFIX.replace(filter, "").to_lowercase()
        } else {
            filter.to_lowercase()
        };
        let candidates: Vec<_> = self
            .candidates
            .iter()
            .filter(|c| {
                c.invocation.kind == query.kind
                    && (filter.is_empty()
                        || c.invocation.name.to_lowercase().contains(&filter)
                        || c.description.to_lowercase().contains(&filter))
            })
            .cloned()
            .collect();
        let status = if self.loading && candidates.is_empty() {
            Some(self.language.text("候補を読み込み中…").into())
        } else if let Some(error) = &self.error {
            Some(error.clone())
        } else if candidates.is_empty() {
            Some(self.language.text("該当する候補がありません").into())
        } else {
            None
        };
        Some(ComposerSuggestions {
            candidates,
            status,
            query_key,
        })
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
    text: String,
    cursor: u32,
    kind: InvocationKind,
    name: String,
) -> Option<ComposerInsertion> {
    let query = query(&text, cursor as usize)?;
    if kind != query.kind {
        return None;
    }
    let token = format!("{}{name}", kind.sigil());
    let range = query.range;
    Some(insert_completion(text, range, &token))
}

// Both invocation tokens and file links consume exactly one following ASCII
// separator. Additional spaces and every non-ASCII separator remain intact.
fn insert_completion(
    mut text: String,
    mut range: std::ops::Range<usize>,
    token: &str,
) -> ComposerInsertion {
    let cursor = range.start + token.len() + 1;
    if text.as_bytes().get(range.end) == Some(&b' ') {
        range.end += 1;
    }
    text.replace_range(range, &format!("{token} "));
    ComposerInsertion {
        text,
        cursor: cursor as u32,
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionBoundary {
    Stop,
    Wrap,
}

/// The TSX source menu cycles at its edges and dismisses even empty results.
/// Existing bounded native lists keep their own edge behavior explicitly.
pub fn completion_key(
    key: &str,
    highlighted: usize,
    count: usize,
    boundary: CompletionBoundary,
) -> Option<CompletionKey> {
    if key == "escape" && boundary == CompletionBoundary::Wrap {
        return Some(CompletionKey::Dismiss);
    }
    let last = count.checked_sub(1)?;
    let index = match boundary {
        CompletionBoundary::Stop => highlighted.min(last),
        CompletionBoundary::Wrap => {
            if highlighted < count {
                highlighted
            } else {
                0
            }
        }
    };
    Some(match key {
        "up" => CompletionKey::Highlight(if index == 0 && boundary == CompletionBoundary::Wrap {
            last
        } else {
            index.saturating_sub(1)
        }),
        "down" => {
            CompletionKey::Highlight(if index == last && boundary == CompletionBoundary::Wrap {
                0
            } else {
                (index + 1).min(last)
            })
        }
        "return" | "tab" => CompletionKey::Insert(index),
        "escape" => CompletionKey::Dismiss,
        _ => return None,
    })
}

/// Keep a selected item only within the same query and current result set.
/// Empty async results retire it; restored results start at their first item.
pub fn completion_selection(
    current_query: Option<&str>,
    highlighted_query: Option<&str>,
    highlighted: Option<&str>,
    keys: &[String],
) -> Option<String> {
    if current_query == highlighted_query
        && let Some(key) = highlighted.filter(|key| keys.iter().any(|candidate| candidate == key))
    {
        return Some(key.into());
    }
    keys.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, Event, Intent, Snapshot, reduce};

    #[test]
    fn source_completion_keys_cycle_and_dismiss_an_empty_menu() {
        for (key, highlighted, count, expected) in [
            ("up", 0, 3, Some(CompletionKey::Highlight(2))),
            ("down", 2, 3, Some(CompletionKey::Highlight(0))),
            ("up", 1, 3, Some(CompletionKey::Highlight(0))),
            ("down", 0, 3, Some(CompletionKey::Highlight(1))),
            ("escape", 0, 0, Some(CompletionKey::Dismiss)),
            ("return", 0, 0, None),
            ("return", 3, 3, Some(CompletionKey::Insert(0))),
            ("return", 7, 3, Some(CompletionKey::Insert(0))),
        ] {
            assert_eq!(
                completion_key(key, highlighted, count, CompletionBoundary::Wrap),
                expected
            );
        }
    }

    #[test]
    fn completion_selection_uses_query_and_item_identity_and_retires_async_emptiness() {
        let keys = vec!["first".into(), "second".into()];
        assert_eq!(
            completion_selection(Some("q"), Some("q"), Some("second"), &keys),
            Some("second".into())
        );
        let reversed = vec!["second".into(), "first".into()];
        assert_eq!(
            completion_selection(Some("q"), Some("q"), Some("second"), &reversed),
            Some("second".into())
        );
        assert_eq!(
            completion_selection(Some("new"), Some("q"), Some("second"), &keys),
            Some("first".into())
        );
        assert_eq!(
            completion_selection(Some("q"), Some("q"), Some("missing"), &keys),
            Some("first".into())
        );
        let retired = completion_selection(Some("q"), Some("q"), Some("second"), &[]);
        assert_eq!(retired, None);
        assert_eq!(
            completion_selection(Some("q"), Some("q"), retired.as_deref(), &keys),
            Some("first".into())
        );
    }

    #[test]
    fn completion_query_identity_is_normalized_raw_query_not_position_or_matcher_text() {
        let source = completion_catalog(UiLanguage::English, None, None);
        let key = |text: &str| {
            source
                .suggestions(text, text.len() as u32)
                .unwrap()
                .query_key
        };
        assert_eq!(key("one $REV"), key("two €rev"));
        assert_eq!(key("$\u{a0}REV\u{a0}"), key("$rev"));
        assert_ne!(key("$rev"), key("/rev"));
        assert_ne!(key("$rev"), key("$$rev"));
    }

    proptest::proptest! {
        #[test]
        fn completion_keys_never_address_a_missing_result(highlighted in proptest::prelude::any::<usize>(), count in 0usize..64) {
            for boundary in [CompletionBoundary::Stop, CompletionBoundary::Wrap] {
                for key in ["up", "down", "return", "tab", "escape"] {
                    if let Some(CompletionKey::Highlight(index) | CompletionKey::Insert(index)) = completion_key(key, highlighted, count, boundary) {
                        proptest::prop_assert!(index < count);
                    }
                }
            }
        }
    }

    // Pinned composer-logic.ts detects /^\p{Sc}/u, and shared/composerTrigger.ts
    // rejects money while still allowing skill names beginning with digits.
    #[rstest::rstest]
    #[case("$rev", Some("rev"))]
    #[case("€review", Some("review"))]
    #[case("£", Some(""))]
    #[case("￥レビュー", Some("レビュー"))]
    #[case("＄review/name", Some("review/name"))]
    #[case("$[review]", Some("[review]"))]
    #[case("$123skill", Some("123skill"))]
    #[case("$1e-3", Some("1e-3"))]
    #[case("$500", None)]
    #[case("€1,234.50", None)]
    #[case("£.5", None)]
    #[case("￥2k", None)]
    #[case("$1e3", None)]
    #[case("$12.00!", None)]
    #[case("cost$review", None)]
    #[case("\u{a0}$review", None)]
    #[case("$review\u{a0}tail", Some("review\u{a0}tail"))]
    #[case("$review\u{2003}tail", Some("review\u{2003}tail"))]
    #[case("$review\u{b}", Some("review\u{b}"))]
    #[case("$500\u{a0}", Some("500\u{a0}"))]
    fn currency_skill_trigger_follows_the_pinned_source(
        #[case] text: &str,
        #[case] expected: Option<&str>,
    ) {
        let found = query(text, text.len());
        assert_eq!(found.as_ref().map(|query| query.filter), expected);
        if let Some(query) = found {
            assert_eq!(query.range, 0..text.len());
            assert_eq!(query.kind, InvocationKind::Skill);
        }
    }

    #[test]
    fn currency_skill_search_trims_ecmascript_space_instead_of_changing_token_boundaries() {
        let catalog = ComposerCatalog {
            candidates: vec![ComposerCandidate {
                invocation: skill(),
                description: "Review changes".into(),
            }],
            ..Default::default()
        };
        let source =
            completion_catalog(UiLanguage::English, Some(&catalog), Some(skill().provider));
        for (text, count) in [
            ("$\u{a0}REV\u{a0}", 1),
            ("€\u{feff}review\u{feff}", 1),
            ("$$review", 1),
            ("$\u{85}review\u{85}", 0),
        ] {
            let menu = source.suggestions(text, text.len() as u32).unwrap();
            assert_eq!(menu.candidates.len(), count, "{text:?}");
        }
    }

    #[test]
    fn currency_invocations_preserve_the_ascii_token_boundary_and_suffix() {
        for (text, before, start, expected, caret) in [
            (
                "prefix $rev suffix",
                "prefix $rev",
                7,
                "prefix $review suffix",
                15,
            ),
            (
                "日本語\t€rev tail",
                "日本語\t€rev",
                10,
                "日本語\t$review tail",
                18,
            ),
            ("x\n￥rev tail", "x\n￥rev", 2, "x\n$review tail", 10),
            ("x\r£rev", "x\r£rev", 2, "x\r$review ", 10),
        ] {
            let query = query(text, before.len()).unwrap();
            assert_eq!(query.range, start..before.len());
            assert_eq!(query.filter, "rev");
            let insertion =
                insert_invocation(text.into(), before.len() as u32, skill().kind, skill().name)
                    .unwrap();
            assert_eq!(insertion.text, expected);
            assert_eq!(insertion.cursor, caret);
        }
    }

    #[test]
    fn native_catalog_queries_preserve_provider_status_and_caret_boundaries() {
        use agent_protocol::session::ProviderKind::{Claude, Codex};
        let candidate = |provider, kind, name: &str| ComposerCandidate {
            invocation: Invocation {
                provider,
                kind,
                name: name.into(),
                path: format!("/{name}"),
            },
            description: format!("Use {name}"),
        };
        let mut catalog = ComposerCatalog {
            candidates: vec![
                candidate(Codex, InvocationKind::Skill, "review"),
                candidate(Claude, InvocationKind::Skill, "review"),
                candidate(Codex, InvocationKind::Plugin, "plugin"),
            ],
            errors: [(Codex, vec!["Catalog warning".into()])].into(),
            loading: true,
            ..Default::default()
        };
        let source = completion_catalog(UiLanguage::English, Some(&catalog), Some(Codex));
        let menu = source.suggestions("前😀 /REV tail", 12).unwrap();
        assert_eq!(menu.candidates, vec![catalog.candidates[0].clone()]);
        assert_eq!(menu.status.as_deref(), Some("Catalog warning"));
        assert_eq!(
            source.suggestions("/missing", 8).unwrap().status.as_deref(),
            Some("Loading suggestions…")
        );
        assert!(source.suggestions("前😀 /REV tail", 15).is_none());
        assert!(
            source.suggestions("前😀 /REV tail", 4).is_none(),
            "invalid UTF-8 boundary"
        );
        assert_eq!(
            source.suggestions("@pl suffix", 3).unwrap().candidates,
            vec![catalog.candidates[2].clone()]
        );
        catalog.loading = false;
        let source = completion_catalog(UiLanguage::English, Some(&catalog), Some(Codex));
        assert_eq!(
            source.suggestions("/missing", 8).unwrap().status.as_deref(),
            Some("Catalog warning")
        );
        catalog.errors.clear();
        let source = completion_catalog(UiLanguage::English, Some(&catalog), Some(Codex));
        assert_eq!(
            source.suggestions("/missing", 8).unwrap().status.as_deref(),
            Some("No matching suggestions")
        );
    }

    #[rstest::rstest]
    #[case(0, Some(0))]
    #[case(1, Some(3))]
    #[case(2, None)]
    #[case(3, Some(7))]
    #[case(4, Some(8))]
    #[case(5, None)]
    fn native_caret_converts_only_complete_unicode_boundaries(
        #[case] units: u32,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(
            agent_protocol::context::byte_offset("前😀a", units),
            expected
        );
    }

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
        assert_eq!(
            completion_key(key, highlighted, count, CompletionBoundary::Stop),
            expected
        );
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
            Event::from(Intent::SetDraft {
                thread_id: "origin".into(),
                draft: draft.clone(),
            }),
        );
        let (snapshot, _) = reduce(
            &snapshot,
            "",
            Event::from(Intent::SetDraftText {
                thread_id: "other".into(),
                text: "別の下書き".into(),
            }),
        );
        let (snapshot, _) = reduce(
            &snapshot,
            "",
            Event::from(Intent::NewChat {
                cwd: "/other".into(),
            }),
        );
        let (next, effects) = reduce(
            &snapshot,
            "",
            Event::from(Intent::QuoteSelection {
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
            Event::from(Intent::QuoteSelection {
                storage_scope: "previous-account".into(),
                thread_id: "origin".into(),
                text: "古い選択".into(),
            }),
        );
        assert_eq!(stale.input.drafts, snapshot.input.drafts);
        assert!(effects.is_empty());
    }

    #[test]
    fn a_selection_from_another_account_does_not_create_a_welcome_draft() {
        let previous = Snapshot::default();
        let (next, effects) = reduce(
            &previous,
            "",
            Event::from(Intent::QuoteSelection {
                storage_scope: "previous-account".into(),
                thread_id: previous.panes.primary().navigation.draft_key.clone(),
                text: "stale selection".into(),
            }),
        );
        assert_eq!(next.input, previous.input);
        assert_eq!(next.panes, previous.panes);
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
        assert_eq!(inserted.text, "日本語 $review 後半");
        assert_eq!(inserted.cursor as usize, "日本語 $review ".len());
        assert_eq!(
            query("＠rev", "＠rev".len()).unwrap().kind,
            InvocationKind::Plugin
        );
        assert_eq!(
            query("／rev", "／rev".len()).unwrap().kind,
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

    // Pinned ChatComposer's skill insertion uses
    // composer-logic::extendReplacementRangeForTrailingSpace: consume exactly
    // one existing ASCII space. Additional or different whitespace is content.
    #[rstest::rstest]
    #[case("", "前😀 $review ")]
    #[case(" tail", "前😀 $review tail")]
    #[case("  tail", "前😀 $review  tail")]
    #[case("\ttail", "前😀 $review \ttail")]
    #[case("\ntail", "前😀 $review \ntail")]
    #[case("\u{a0}tail", "前😀 $review \u{a0}tail")]
    #[case("後半", "前😀 $review 後半")]
    fn completion_replaces_one_existing_ascii_separator(
        #[case] suffix: &str,
        #[case] expected: &str,
    ) {
        let prefix = "前😀 /rev";
        let inserted = insert_invocation(
            format!("{prefix}{suffix}"),
            prefix.len() as u32,
            skill().kind,
            skill().name,
        )
        .unwrap();
        assert_eq!(inserted.text, expected);
        assert_eq!(inserted.cursor, "前😀 $review ".len() as u32);
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
                Event::from(Intent::SetDraftText {
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
            Event::from(Intent::EditComposer {
                thread_id: key.clone(),
                text: "/".into(),
                cursor: 1,
            }),
        );
        let key = snapshot.panes.primary().navigation.draft_key.clone();
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
            Event::from(Intent::EditComposer {
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
            Event::from(Intent::SetDraft {
                thread_id: key.clone(),
                draft,
            }),
        )
        .0;
        let snapshot = reduce(
            &snapshot,
            "",
            Event::from(Intent::InsertInvocation {
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
            Event::from(Intent::NewChat {
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
            Event::from(Intent::EditComposer {
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
            Event::from(Intent::NewChat {
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
                Event::from(Intent::InsertInvocation {
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
