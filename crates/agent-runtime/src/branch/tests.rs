//! TextGenerationPrompts.test.ts "buildBranchNamePrompt" and the shared git
//! helpers' tests (formatGeneratedBranchName, isTemporaryWorktreeBranch).
use super::*;
use agent_domain::AttachmentKind;
use proptest::prelude::*;

fn naming(mode: BranchNamingMode, prefix: &str, instructions: &str) -> BranchNaming {
    BranchNaming {
        mode,
        prefix: prefix.into(),
        instructions: instructions.into(),
    }
}

#[test]
fn requests_a_semantic_prefix_as_part_of_the_same_branch_response() {
    let prompt = branch_name_prompt(
        &naming(BranchNamingMode::Semantic, "ignored", "ignored instruction"),
        "Add search",
        &[],
    );
    assert!(prompt.contains("feat/add-search"));
    assert!(!prompt.contains("ignored instruction"));
    assert_eq!(branch_name_output_schema()["required"], json!(["branch"]));
}

#[test]
fn appends_custom_instructions_without_imposing_a_prefix_case_or_word_limit() {
    let prompt = branch_name_prompt(
        &naming(
            BranchNamingMode::Custom,
            "ignored",
            "Use Julius/ABC-123 and preserve capitalization.",
        ),
        "Add search",
        &[],
    );
    assert!(prompt.contains("Use Julius/ABC-123 and preserve capitalization."));
    assert!(prompt.contains("complete branch name"));
    assert!(!prompt.contains("2-6 words"));
    assert!(!prompt.contains("lowercase"));
    assert!(!prompt.contains("no issue prefixes"));
}

#[test]
fn asks_for_just_the_fragment_in_static_mode() {
    let prompt = branch_name_prompt(
        &naming(BranchNamingMode::Static, "team", "ignored instruction"),
        "Add search",
        &[],
    );
    assert!(prompt.contains("without a prefix or namespace"));
    assert!(!prompt.contains("ignored instruction"));
}

#[test]
fn includes_the_user_message_in_the_prompt() {
    let prompt = branch_name_prompt(&BranchNaming::default(), "Fix the login timeout bug", &[]);
    assert!(prompt.contains("User message:"));
    assert!(prompt.contains("Fix the login timeout bug"));
    assert!(!prompt.contains("Attachment metadata:"));
}

#[test]
fn includes_attachment_metadata_when_attachments_are_provided() {
    let prompt = branch_name_prompt(
        &BranchNaming::default(),
        "Fix the layout from screenshot",
        &[Attachment {
            kind: AttachmentKind::Image,
            source: None,
            id: "att-123".into(),
            name: "screenshot.png".into(),
            mime_type: "image/png".into(),
            path: String::new(),
            size: 12345,
        }],
    );
    assert!(prompt.contains("Attachment metadata:"));
    assert!(prompt.contains("screenshot.png"));
    assert!(prompt.contains("image/png"));
    assert!(prompt.contains("12345 bytes"));
}

// The whole prompt, line for line, as buildPromptFromMessage joins it.
#[test]
fn the_static_prompt_matches_the_reference_layout() {
    assert_eq!(
        branch_name_prompt(&BranchNaming::default(), "Add search", &[]),
        [
            "You generate concise git branch names.",
            "Return a JSON object with key: branch.",
            "Rules:",
            "- Branch should describe the requested work from the user message.",
            "- Return a valid Git branch name without spaces.",
            "- Keep it short and specific (2-6 words), in lowercase with hyphen-separated words.",
            "- Return only the descriptive branch fragment, without a prefix or namespace. The application adds the configured prefix.",
            "- If images are attached, use them as primary context for visual/UI issues.",
            "",
            "User message:",
            "Add search",
        ]
        .join("\n")
    );
}

#[test]
fn joins_a_static_prefix_with_one_slash() {
    for prefix in ["agent", "agent/"] {
        assert_eq!(
            format_generated_branch_name(
                "Add Search",
                &naming(BranchNamingMode::Static, prefix, "")
            ),
            "agent/add-search"
        );
    }
}

#[test]
fn supports_an_empty_prefix_and_preserves_user_prefix_casing() {
    assert_eq!(
        format_generated_branch_name("Add Search", &naming(BranchNamingMode::Static, "", "")),
        "add-search"
    );
    assert_eq!(
        format_generated_branch_name(
            "Add Search",
            &naming(BranchNamingMode::Static, "Team/Julius/", "")
        ),
        "Team/Julius/add-search"
    );
}

#[test]
fn normalizes_invalid_static_prefixes() {
    for (prefix, expected) in [
        ("release..candidate", "release-candidate/add-search"),
        (" Team / Jules.lock/", "Team/Jules-lock/add-search"),
        ("-team//feature@{new}", "team/feature-new/add-search"),
        (" /?. / ", "add-search"),
    ] {
        assert_eq!(
            format_generated_branch_name(
                "Add Search",
                &naming(BranchNamingMode::Static, prefix, "")
            ),
            expected,
            "{prefix:?}"
        );
    }
}

#[test]
fn uses_the_models_semantic_prefix_without_the_stored_static_prefix() {
    assert_eq!(
        format_generated_branch_name(
            "feat/Add Search",
            &naming(BranchNamingMode::Semantic, "agent", "")
        ),
        "feat/add-search"
    );
}

#[test]
fn preserves_the_full_custom_name_including_case_dots_and_length() {
    let branch = format!("Julius/ABC-123/release.v2-{}", "x".repeat(70));
    assert_eq!(
        format_generated_branch_name(
            &format!(" {branch} "),
            &naming(BranchNamingMode::Custom, "ignored", "")
        ),
        branch
    );
}

#[test]
fn reads_the_branch_from_the_structured_answer() {
    let static_naming = BranchNaming::default();
    assert_eq!(
        generated_branch_name(r#"{"branch":"Fix Login"}"#, &static_naming).as_deref(),
        Some("agent/fix-login")
    );
    assert_eq!(generated_branch_name("fix-login", &static_naming), None);
    assert_eq!(
        generated_branch_name(r#"{"title":"x"}"#, &static_naming),
        None
    );
}

#[test]
fn matches_the_temporary_launch_branches() {
    assert!(is_temporary_worktree_branch("agent/session-0123456789ab"));
    assert!(is_temporary_worktree_branch(" agent/session-0123456789ab "));
    assert!(is_temporary_worktree_branch("agent/session-0123456789AB"));
    assert!(!is_temporary_worktree_branch("agent/session-0123456789a"));
    assert!(!is_temporary_worktree_branch("agent/session-0123456789abc"));
    assert!(!is_temporary_worktree_branch("agent/session-0123456789ag"));
    assert!(!is_temporary_worktree_branch("agent/feature/demo"));
    assert!(!is_temporary_worktree_branch("main"));
}

proptest! {
    // sanitizeBranchFragment's contract, from its specification: lowercase
    // `[a-z0-9/_-]`, at most 64 characters, no empty path component, no
    // separator at either end, and `update` when nothing remains.
    #[test]
    fn sanitized_fragments_are_short_lowercase_branch_paths(raw in "\\PC{0,100}") {
        let fragment = sanitize_branch_fragment(&raw);
        prop_assert!(!fragment.is_empty() && fragment.len() <= 64, "{:?}", fragment);
        prop_assert!(fragment.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"/_-".contains(&b)));
        prop_assert!(!fragment.contains("//") && !fragment.contains("--"));
        let separator = |c: char| matches!(c, '/' | '_' | '-');
        prop_assert!(!fragment.starts_with(separator) && !fragment.ends_with(separator));
    }

    // Words of lowercase letters and digits survive as hyphenated words.
    #[test]
    fn plain_words_become_a_hyphenated_fragment(words in prop::collection::vec("[a-z0-9]{1,8}", 1..5)) {
        let expected = words.join("-");
        prop_assume!(expected.len() <= 64);
        prop_assert_eq!(sanitize_branch_fragment(&words.join(" ")), expected);
    }

    // A static name is the cleaned prefix's non-empty parts, then the fragment.
    #[test]
    fn static_names_are_the_prefix_parts_then_the_fragment(prefix in "\\PC{0,30}", raw in "\\PC{0,30}") {
        let name = format_generated_branch_name(&raw, &naming(BranchNamingMode::Static, &prefix, ""));
        let fragment = sanitize_branch_fragment(&raw);
        prop_assert!(name.ends_with(&fragment));
        let head = &name[..name.len() - fragment.len()];
        prop_assert!(head.is_empty() || head.ends_with('/'));
        for part in head.split('/').filter(|part| !part.is_empty()) {
            prop_assert!(part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
            prop_assert!(!part.starts_with('-') && !part.ends_with('-') && !part.contains("--"));
        }
        prop_assert!(!head.contains("//"));
    }
}
