use super::*;
use crate::sync::fixtures::thread_state;
use agent_domain::{ThreadId, shell};
use rstest::rstest;

// client-runtime providerSkills.ts resolveProviderSkillSourceKind.
#[test]
fn skill_sources_come_from_plugin_paths_then_the_scope() {
    assert_eq!(
        skill_source_kind("/home/me/.codex/plugins/x/SKILL.md", Some("user")),
        SkillSourceKind::App
    );
    assert_eq!(
        skill_source_kind("C:\\Users\\me\\.agents\\plugins\\x", None),
        SkillSourceKind::App
    );
    assert_eq!(
        skill_source_kind("/r/a", Some(" Repository ")),
        SkillSourceKind::Repo
    );
    assert_eq!(
        skill_source_kind("/r/a", Some("workspace")),
        SkillSourceKind::Project
    );
    assert_eq!(
        skill_source_kind("/r/a", Some("user")),
        SkillSourceKind::Personal
    );
    assert_eq!(
        skill_source_kind("/r/a", Some("system")),
        SkillSourceKind::System
    );
    assert_eq!(skill_source_kind("/r/a", None).label(), "Provider");
}

fn utf16_len(text: &str) -> u32 {
    crate::js_text::utf16_len(text) as u32
}

fn trigger(text: &str) -> Option<ComposerTrigger> {
    detect_composer_trigger(text, utf16_len(text))
}

fn found(
    kind: ComposerTriggerKind,
    query: &str,
    start: &str,
    end: &str,
) -> Option<ComposerTrigger> {
    Some(ComposerTrigger {
        kind,
        query: query.into(),
        range_start: utf16_len(start),
        range_end: utf16_len(end),
    })
}

#[rstest]
fn detects_skill_prefixes_and_their_source_range(
    #[values("$", "€", "£", "¥", "₹", "₩", "₿", "𑿝")] prefix: &str,
) {
    let text = format!("Use {prefix}review");
    assert_eq!(
        trigger(&text),
        found(ComposerTriggerKind::Skill, "review", "Use ", &text)
    );
}

#[test]
fn uses_the_basename_as_the_markdown_label() {
    assert_eq!(
        serialize_composer_file_link("path/to/package.json"),
        "[package.json](path/to/package.json)"
    );
}

#[test]
fn encodes_markdown_sensitive_destination_characters() {
    assert_eq!(
        serialize_composer_file_link("docs/My File (draft).md"),
        "[My File (draft).md](docs/My%20File%20%28draft%29.md)"
    );
}

#[test]
fn supports_windows_paths() {
    assert_eq!(
        serialize_composer_file_link("C:\\repo\\src\\index.ts"),
        "[index.ts](C:%5Crepo%5Csrc%5Cindex.ts)"
    );
}

#[test]
fn preserves_paths_that_legitimately_start_with_an_at_sign() {
    assert_eq!(
        serialize_composer_file_link("@scope/package.json"),
        "[package.json](@scope/package.json)"
    );
}

#[test]
fn detects_at_path_trigger_at_cursor() {
    let text = "Please check @src/com";
    assert_eq!(
        trigger(text),
        found(ComposerTriggerKind::Path, "src/com", "Please check ", text)
    );
}

#[test]
fn detects_slash_command_token_while_typing_command_name() {
    assert_eq!(
        trigger("/mo"),
        found(ComposerTriggerKind::SlashCommand, "mo", "", "/mo")
    );
}

#[test]
fn detects_non_model_slash_commands_while_typing() {
    assert_eq!(
        trigger("/pl"),
        found(ComposerTriggerKind::SlashCommand, "pl", "", "/pl")
    );
}

#[test]
fn keeps_slash_command_detection_active_for_provider_commands() {
    assert_eq!(
        trigger("/rev"),
        found(ComposerTriggerKind::SlashCommand, "rev", "", "/rev")
    );
}

#[test]
fn reports_the_model_command_and_its_query() {
    assert_eq!(
        trigger("/model"),
        found(ComposerTriggerKind::SlashModel, "", "", "/model")
    );
    assert_eq!(
        trigger("/model spark"),
        found(ComposerTriggerKind::SlashModel, "spark", "", "/model spark")
    );
}

#[rstest]
fn detects_skill_trigger_at_cursor(
    #[values("$", "€", "£", "¥", "₹", "₩", "₿", "𑿝")] prefix: &str
) {
    let text = format!("Use {prefix}gh-fi");
    assert_eq!(
        trigger(&text),
        found(ComposerTriggerKind::Skill, "gh-fi", "Use ", &text)
    );
}

#[test]
fn detects_a_pull_request_number_at_a_token_boundary() {
    let text = "Compare this with #8737";
    assert_eq!(
        trigger(text),
        found(
            ComposerTriggerKind::PullRequest,
            "8737",
            "Compare this with ",
            text
        )
    );
}

#[test]
fn opens_pull_request_completion_from_a_bare_hash() {
    let text = "Compare with #";
    assert_eq!(
        trigger(text),
        found(ComposerTriggerKind::PullRequest, "", "Compare with ", text)
    );
}

#[test]
fn detects_a_one_word_pull_request_search() {
    let text = "Compare with #composer";
    assert_eq!(
        trigger(text),
        found(
            ComposerTriggerKind::PullRequest,
            "composer",
            "Compare with ",
            text
        )
    );
}

#[test]
fn supports_hyphenated_pull_request_search_terms() {
    let text = "Find #inline-context";
    assert_eq!(
        trigger(text),
        found(
            ComposerTriggerKind::PullRequest,
            "inline-context",
            "Find ",
            text
        )
    );
}

#[test]
fn does_not_keep_pull_request_completion_active_for_headings_or_embedded_hashes() {
    assert_eq!(trigger("# Heading"), None);
    assert_eq!(trigger("issue#123"), None);
}

#[test]
fn detects_at_path_trigger_in_the_middle_of_existing_text() {
    let text = "Please inspect @in this sentence";
    let cursor = "Please inspect @";
    assert_eq!(
        detect_composer_trigger(text, utf16_len(cursor)),
        found(ComposerTriggerKind::Path, "", "Please inspect ", cursor)
    );
}

#[test]
fn detects_at_path_trigger_with_query_typed_mid_text() {
    let text = "Please inspect @srin this sentence";
    let cursor = "Please inspect @sr";
    assert_eq!(
        detect_composer_trigger(text, utf16_len(cursor)),
        found(ComposerTriggerKind::Path, "sr", "Please inspect ", cursor)
    );
}

#[test]
fn detects_trigger_with_true_cursor_even_when_mention_detection_would_false_match() {
    let text = "Please inspect @in this sentence";
    let trigger = detect_composer_trigger(text, utf16_len("Please inspect @")).unwrap();
    assert_eq!(trigger.kind, ComposerTriggerKind::Path);
    assert_eq!(trigger.query, "");
}

#[test]
fn replaces_a_text_range_and_returns_new_cursor() {
    assert_eq!(
        replace_text_range("hello @src", 6, 10, ""),
        TextReplacement {
            text: "hello ".into(),
            cursor: 6,
        }
    );
}

#[test]
fn double_space_after_insertion_when_replacement_ends_with_space() {
    let text = "and then @AG summarize";
    let start = utf16_len("and then ");
    let end = utf16_len("and then @AG");
    let without = replace_text_range(text, start, end, "@AGENTS.md ");
    assert_eq!(without.text, "and then @AGENTS.md  summarize");
    let extended = if text.as_bytes()[end as usize] == b' ' {
        end + 1
    } else {
        end
    };
    let with = replace_text_range(text, start, extended, "@AGENTS.md ");
    assert_eq!(with.text, "and then @AGENTS.md summarize");
}

#[test]
fn parses_standalone_plan_command() {
    assert_eq!(
        parse_standalone_slash_command(" /plan "),
        Some(InteractionMode::Plan)
    );
}

#[test]
fn parses_standalone_default_command() {
    assert_eq!(
        parse_standalone_slash_command("/default"),
        Some(InteractionMode::Default)
    );
}

#[test]
fn ignores_slash_commands_with_extra_message_text() {
    assert_eq!(parse_standalone_slash_command("/plan explain this"), None);
}

fn menu<'a>(
    trigger: &'a ComposerTrigger,
    slash_commands: &'a [ProviderSlashCommand],
    allow_interaction_mode: bool,
) -> ComposerCommandMenuInput<'a> {
    ComposerCommandMenuInput {
        trigger,
        driver: Some(Driver::Claude),
        has_thread: true,
        has_compactable_conversation: false,
        allow_interaction_mode,
        skills: &[],
        slash_commands,
        path_entries: &[],
        threads: &[],
        current_thread: None,
    }
}

fn command(name: &str, description: Option<&str>) -> ProviderSlashCommand {
    ProviderSlashCommand {
        name: name.into(),
        description: description.map(Into::into),
    }
}

fn slash_trigger_for(query: &str, range_start: u32) -> ComposerTrigger {
    ComposerTrigger {
        kind: ComposerTriggerKind::SlashCommand,
        query: query.into(),
        range_start,
        range_end: range_start + utf16_len(query) + 1,
    }
}

fn select(
    draft: &str,
    end: u32,
    item: &ComposerCommandItem,
    allow_interaction_mode: bool,
) -> ComposerCommandSelection {
    let trigger = ComposerTrigger {
        kind: ComposerTriggerKind::SlashCommand,
        query: String::new(),
        range_start: 0,
        range_end: end,
    };
    resolve_composer_command_selection(draft, &trigger, item, allow_interaction_mode, &[]).unwrap()
}

// A provider that hides the mode toggle (allow_interaction_mode false) keeps
// its own `/plan`.
#[rstest]
fn keeps_native_plan_with_legacy_mode(#[values(false, true)] allow_interaction_mode: bool) {
    let commands = [command("plan", Some("Plan natively"))];
    let trigger = slash_trigger_for("pl", 0);
    let items = slash_command_items("pl", true, &menu(&trigger, &commands, false));
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].target,
        ComposerCommandTarget::ProviderCommand {
            name: "plan".into()
        }
    );
    assert_eq!(
        select("/pl", 3, &items[0], allow_interaction_mode),
        ComposerCommandSelection {
            text: "/plan ".into(),
            cursor: 6,
            interaction_mode: None,
            attach_thread: None,
        }
    );
}

#[test]
fn does_not_offer_a_native_command_inside_the_message() {
    let commands = [command("plan", Some("Plan natively"))];
    let trigger = slash_trigger_for("plan", 4);
    let input = ComposerCommandMenuInput {
        has_thread: false,
        ..menu(&trigger, &commands, false)
    };
    assert_eq!(slash_command_items("plan", false, &input), vec![]);
}

#[test]
fn still_applies_the_built_in_plan_command_for_supported_providers() {
    let trigger = slash_trigger_for("plan", 0);
    let input = ComposerCommandMenuInput {
        driver: Some(Driver::Codex),
        ..menu(&trigger, &[], true)
    };
    let items = slash_command_items("plan", true, &input);
    assert_eq!(
        select("/plan", 5, &items[0], true),
        ComposerCommandSelection {
            text: String::new(),
            cursor: 0,
            interaction_mode: Some(InteractionMode::Plan),
            attach_thread: None,
        }
    );
    // A provider switch can invalidate an open menu before a tap arrives.
    assert_eq!(
        select("/plan", 5, &items[0], false),
        ComposerCommandSelection {
            text: "/plan ".into(),
            cursor: 6,
            interaction_mode: None,
            attach_thread: None,
        }
    );
}

#[test]
fn offers_compact_only_for_a_compactable_conversation() {
    let commands = [command("compact", None), command("feedback", None)];
    let trigger = slash_trigger_for("", 0);
    let names = |input: &ComposerCommandMenuInput<'_>| {
        slash_command_items("", true, input)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>()
    };
    let input = menu(&trigger, &commands, true);
    assert_eq!(names(&input), ["/model", "/plan", "/default", "/feedback"]);
    let compactable = ComposerCommandMenuInput {
        has_compactable_conversation: true,
        ..input
    };
    assert_eq!(
        names(&compactable),
        ["/model", "/plan", "/default", "/compact", "/feedback"]
    );
    let new_codex_task = ComposerCommandMenuInput {
        driver: Some(Driver::Codex),
        has_thread: false,
        ..compactable
    };
    assert_eq!(
        names(&new_codex_task),
        ["/model", "/plan", "/default", "/compact"]
    );
}

#[test]
fn compactable_conversation_needs_a_message_other_than_a_bare_compact() {
    let mut state = thread_state("Thread");
    assert!(!has_compactable_conversation(&state, false, None));
    let compact = Message {
        notification: None,
        id: agent_domain::MessageId::new("m1").unwrap(),
        run: None,
        role: Role::User,
        text: " /COMPACT ".into(),
        attachments: vec![],
        intent: agent_domain::InputIntent::TurnStart,
        streaming: false,
        created_by: agent_domain::MessageAuthor::User,
        creation_source: "test".into(),
        created_at: crate::sync::fixtures::at(),
        updated_at: crate::sync::fixtures::at(),
        context: None,
    };
    state.messages.push(compact.clone());
    assert!(!has_compactable_conversation(&state, false, None));
    assert!(has_compactable_conversation(
        &state,
        true,
        Some(&crate::sync::fixtures::at())
    ));
    state.messages.push(Message {
        text: "hello".into(),
        ..compact
    });
    assert!(has_compactable_conversation(&state, false, None));
}

fn skill(name: &str, short_description: Option<&str>) -> ComposerSkill {
    ComposerSkill {
        name: name.into(),
        short_description: short_description.map(Into::into),
        enabled: true,
        ..ComposerSkill::default()
    }
}

#[test]
fn matches_the_rendered_skill_prefix() {
    let browser = skill("browser", Some("Open and control the in-app browser"));
    assert!(matches_slash_skill_query(&browser, "skill"));
    assert!(matches_slash_skill_query(&browser, "skill:brow"));
}

#[test]
fn slash_menu_lists_commands_then_skills_and_hides_commands_shadowed_by_skills() {
    let skills = [
        skill("review", Some("Review the diff")),
        ComposerSkill {
            user_invocable: Some(false),
            ..skill("internal", None)
        },
    ];
    let commands = [command("review", None), command("rewind", None)];
    let trigger = slash_trigger_for("re", 0);
    let input = ComposerCommandMenuInput {
        skills: &skills,
        ..menu(&trigger, &commands, true)
    };
    let items = composer_command_items(&input);
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["/rewind", "skill:review"]);
}

#[test]
fn ranks_skills_for_a_dollar_query() {
    let skills = [
        skill("gh-fix-ci", None),
        skill("fix", None),
        skill("prefix-tool", None),
        ComposerSkill {
            enabled: false,
            ..skill("fixer", None)
        },
    ];
    let trigger = ComposerTrigger {
        kind: ComposerTriggerKind::Skill,
        query: "fix".into(),
        range_start: 0,
        range_end: 4,
    };
    let input = ComposerCommandMenuInput {
        skills: &skills,
        ..menu(&trigger, &[], true)
    };
    let labels: Vec<String> = composer_command_items(&input)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(labels, ["fix", "gh-fix-ci", "prefix-tool"]);
}

#[test]
fn path_items_follow_thread_matches() {
    let mut thread = shell(&thread_state("Fix login")).unwrap();
    thread.id = ThreadId::new("t1").unwrap();
    let threads = [thread];
    let entries = [
        ComposerPathEntry {
            path: "src/login.ts".into(),
            directory: false,
        },
        ComposerPathEntry {
            path: "login".into(),
            directory: true,
        },
    ];
    let trigger = ComposerTrigger {
        kind: ComposerTriggerKind::Path,
        query: "login".into(),
        range_start: 0,
        range_end: 6,
    };
    let input = ComposerCommandMenuInput {
        path_entries: &entries,
        threads: &threads,
        ..menu(&trigger, &[], true)
    };
    let items = composer_command_items(&input);
    let rows: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|item| {
            (
                item.id.as_str(),
                item.label.as_str(),
                item.description.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("thread:t1", "Fix login", "Thread"),
            ("path:src/login.ts", "login.ts", "src"),
            ("path:login", "login", ""),
        ]
    );
}

#[test]
fn offers_nothing_for_a_bare_at_so_the_picker_stays_a_file_picker() {
    let threads = [shell(&thread_state("Fix login")).unwrap()];
    assert_eq!(thread_items(&threads, None, "  "), vec![]);
}

#[test]
fn matches_titles_newest_first_skipping_self_and_archived() {
    let at = |value: &str| Timestamp::parse(value).unwrap();
    let thread = |id: &str, title: &str, updated: &str| {
        let mut row = shell(&thread_state(title)).unwrap();
        row.id = ThreadId::new(id).unwrap();
        row.updated_at = at(updated);
        row
    };
    let mut gone = thread("gone", "Login archived", "2026-01-01T00:00:00Z");
    gone.archived_at = Some(at("2026-01-02T00:00:00Z"));
    let threads = [
        thread("old", "Login flow", "2026-01-01T00:00:00Z"),
        thread("new", "Login redesign", "2026-02-01T00:00:00Z"),
        thread("self", "Login self", "2026-01-01T00:00:00Z"),
        gone,
        thread("nope", "Unrelated", "2026-01-01T00:00:00Z"),
    ];
    let items = thread_items(&threads, Some("self"), "LOGIN");
    let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["thread:new", "thread:old"]);
    assert_eq!(items[0].label, "Login redesign");
}

#[test]
fn choosing_a_thread_inserts_its_link_and_attaches_it_once() {
    let item = ComposerCommandItem {
        id: "thread:t1".into(),
        label: "Fix login".into(),
        description: "Thread".into(),
        skill_source: None,
        target: ComposerCommandTarget::Thread {
            thread_id: "t1".into(),
            title: "Fix login".into(),
        },
    };
    let trigger = ComposerTrigger {
        kind: ComposerTriggerKind::Path,
        query: "fix".into(),
        range_start: 4,
        range_end: 8,
    };
    let selection =
        resolve_composer_command_selection("see @fix", &trigger, &item, true, &[]).unwrap();
    let link = "[Fix login](context://v1/thread/thread_t1) ";
    assert_eq!(selection.text, format!("see {link}"));
    assert_eq!(selection.cursor, utf16_len(&selection.text));
    let attachment = selection.attach_thread.unwrap();
    assert_eq!(attachment.context_id, "thread_t1");
    let record = attachment.record("host");
    assert_eq!(record["threadId"], "t1");
    assert!(
        agent_domain::MessageContext {
            version: 1,
            records: vec![agent_domain::Json(record)],
        }
        .normalized()
        .is_some_and(|context| context.records.len() == 1)
    );

    let again = resolve_composer_command_selection(
        "see @fix",
        &trigger,
        &item,
        true,
        &["thread_t1".into()],
    )
    .unwrap();
    assert_eq!(again.attach_thread, None);

    let full: Vec<String> = (0..agent_domain::COMPOSER_CONTEXT_MAX_RECORDS)
        .map(|index| format!("ctx_{index}"))
        .collect();
    assert_eq!(
        resolve_composer_command_selection("see @fix", &trigger, &item, true, &full),
        Err(TOO_MANY_CONTEXT_ITEMS.into())
    );
}
