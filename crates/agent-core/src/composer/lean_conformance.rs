use super::{
    append_selection_to_draft, code, completions, context, input, snapshot, stash, template,
};
use crate::presentation::markdown::{ArtifactTemplate, ArtifactTemplateKind};
use agent_protocol::{
    composer::{Invocation, InvocationKind},
    context::{CitationSource, TextRange},
    session::{ProviderKind, SessionRef},
    snapshot::SnapShotSource,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fmt::Debug, path::PathBuf};

#[derive(Deserialize)]
struct Row {
    kind: String,
    args: Value,
    expected: Value,
}

fn check<Args, Output>(kind: &str, count: usize, observe: impl Fn(Args) -> Output)
where
    Args: DeserializeOwned,
    Output: DeserializeOwned + PartialEq + Debug,
{
    let directory = std::env::var_os("AGENT_CORE_CASES").expect("set AGENT_CORE_CASES");
    let corpus = std::fs::read_to_string(PathBuf::from(directory).join("composer.jsonl")).unwrap();
    let mut seen = 0;
    for line in corpus.lines() {
        let row: Row = serde_json::from_str(line).unwrap();
        if row.kind == kind {
            let actual = observe(serde_json::from_value(row.args).unwrap());
            assert_eq!(
                actual,
                serde_json::from_value::<Output>(row.expected).unwrap(),
                "{line}"
            );
            seen += 1;
        }
    }
    assert_eq!(seen, count, "incomplete {kind} corpus");
}

#[test]
#[ignore = "requires Lean-generated composer.jsonl"]
fn lean_core_composer_edit_conformance() {
    check(
        "refresh",
        12,
        |(loading, empty, changed): (Option<bool>, bool, bool)| {
            super::should_refresh_catalog(loading, if empty { "" } else { "filter" }, changed)
        },
    );
    check("quote", 6, |(draft, text): (String, String)| {
        append_selection_to_draft(&draft, &text)
    });
    check("path", 6, |[query]: [String; 1]| {
        completions::path_request("/project", &query).query
    });
    check("fence", 18, |[text]: [String; 1]| {
        code::composer_code_fences(&text)
            .into_iter()
            .map(|fence| fence.language)
            .collect::<Vec<_>>()
    });
    check(
        "invocation",
        8,
        |(retained, provider, token): (bool, bool, bool)| {
            let items = [Invocation {
                provider: ProviderKind::Codex,
                kind: InvocationKind::Skill,
                name: "plan".into(),
                path: "/plan".into(),
            }];
            let prior = if retained { items.as_slice() } else { &[] };
            input::selected_invocations(
                &items,
                if token { "$plan" } else { "$planner" },
                provider.then_some(ProviderKind::Codex),
                prior,
            )
            .count()
                == 1
        },
    );
    check("template", 4, |(present, busy): (bool, bool)| {
        let template = ArtifactTemplate {
            artifact_kind: ArtifactTemplateKind::Document,
            display_name: "Report".into(),
            gallery_kind: None,
            skill_directory: "/skills/artifact-template-report".into(),
            skill_name: "artifact-template-report".into(),
        };
        let prompt = "Create a document using this $artifact-template-report about…";
        let draft = if present {
            format!("notes {prompt}\n")
        } else {
            "notes".into()
        };
        match template::append(&draft, &template, busy) {
            Err(_) => "error",
            Ok(text) if text == draft => "unchanged",
            Ok(text) => {
                assert_eq!(text, format!("notes {prompt}"));
                "appended"
            }
        }
        .to_owned()
    });
}

fn stash_result((content, busy, count): (bool, bool, usize)) -> String {
    let draft = crate::state::Draft {
        text: if content { "draft" } else { " " }.into(),
        ..Default::default()
    };
    let previous: Vec<_> = (0..count)
        .map(|index| stash::StashedPrompt {
            id: index.to_string(),
            created_at_ms: 0,
            text: "prior".into(),
            attachments: vec![],
            invocations: vec![],
            contexts: vec![],
        })
        .collect();
    let action = stash::StashAction::Save {
        id: "new".into(),
        created_at_ms: 1,
    };
    match stash::change(&previous, &draft, busy, action) {
        Ok(None) => "unchanged",
        Err(_) => "error",
        Ok(Some(stash::Change::Save {
            entry,
            draft: cleared,
        })) => {
            assert_eq!(entry.text, draft.text);
            assert_eq!(cleared, crate::state::Draft::default());
            "moved"
        }
        _ => panic!("unexpected stash transition"),
    }
    .to_owned()
}

fn session() -> SessionRef {
    SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap()
}

fn capture_result((image, duplicate): (bool, bool)) -> bool {
    let capture = snapshot::SnapShotCapture {
        id: "capture".into(),
        source: SnapShotSource {
            captured_at_ms: 1,
            app_name: "Editor".into(),
            window_title: "Project".into(),
            app_identifier: None,
            app_icon_data_url: None,
            accessibility: None,
        },
    };
    let path = "/host/capture.png".to_owned();
    let previous = if duplicate {
        capture.clone().append(&[], path.clone(), true).unwrap()
    } else {
        vec![]
    };
    capture.append(&previous, path, image).is_ok()
}

fn drop_result((same, invalid): (bool, bool)) -> Option<Vec<String>> {
    let payload = |host: &str, title: &str| {
        let source =
            context::thread_context(host.into(), "storage".into(), session(), title.into())
                .unwrap();
        context::thread_drag_payload(vec![source]).unwrap()
    };
    let mut payloads = vec![
        payload("first", "Old"),
        payload(if same { "first" } else { "second" }, "New"),
    ];
    if invalid {
        payloads.push("[]".into());
    }
    context::thread_drag_contexts(payloads)
        .ok()
        .map(|contexts| contexts.into_iter().map(|item| item.label).collect())
}

fn navigate((first, second): (bool, bool)) -> Option<u32> {
    let source = CitationSource {
        host: "host".into(),
        storage_scope: "storage".into(),
        session: session(),
        turn_id: "turn".into(),
        item_id: "item".into(),
    };
    let range = TextRange { start: 1, end: 3 };
    let citation = context::make_citation(source, "A😀B".into(), range).unwrap();
    let documents = [first, second].map(|found| {
        if found {
            "A😀B".into()
        } else {
            "other".into()
        }
    });
    context::locate_citation_documents(citation, documents.into()).map(|located| {
        assert_eq!(located.range, range);
        located.document
    })
}

#[test]
#[ignore = "requires Lean-generated composer.jsonl"]
fn lean_core_composer_context_conformance() {
    check("stash", 16, stash_result);
    check("capture", 4, capture_result);
    check("navigate", 4, navigate);
    check("drop", 4, drop_result);
}

#[test]
#[ignore = "requires Lean-generated composer.jsonl"]
fn lean_core_localization_conformance() {
    use crate::localization::{UiLanguage, localized_ui_format};
    check(
        "format",
        12,
        |(english, key, value): (bool, String, String)| {
            let language = if english {
                UiLanguage::English
            } else {
                UiLanguage::Japanese
            };
            localized_ui_format(language, key, vec![value])
        },
    );
}

#[test]
#[ignore = "requires Lean-generated composer.jsonl"]
fn lean_core_text_conformance() {
    let regex = regex::Regex::new(crate::text::SPACE).unwrap();
    check("space", 53, |[code]: [u32; 1]| {
        let character = char::from_u32(code).unwrap();
        let single = character.to_string();
        let run = single.repeat(2);
        let space = crate::text::is_space(character);
        assert_eq!(regex.is_match(&single), space);
        let padded = format!("{single}x{single}");
        assert_eq!(
            crate::text::trim(&padded),
            if space { "x" } else { &padded }
        );
        assert_eq!(
            crate::text::trim_end(&padded),
            if space {
                &padded[..padded.len() - single.len()]
            } else {
                &padded
            }
        );
        json!([space, crate::text::collapse_spaces(&run)])
    });
}

#[test]
#[ignore = "requires Lean-generated composer.jsonl"]
fn lean_core_privacy_conformance() {
    check("privacy", 1, |_: [String; 0]| {
        crate::privacy::privacy_policy()
    });
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[test]
#[ignore = "requires Lean-generated composer.jsonl"]
fn lean_core_recording_conformance() {
    check("pcm", 2, |[units]: [Vec<i32>; 1]| {
        let samples = units.into_iter().map(|unit| unit as f32 / 65536.).collect();
        crate::recording::transcription_pcm(samples, 24_000).ok()
    });
}
