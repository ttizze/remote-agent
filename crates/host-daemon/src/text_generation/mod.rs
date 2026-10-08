//! Deterministic prompts and output shaping for source-control text.
//!
//! The provider call itself stays in the Host conversation text generator;
//! this module owns only the prompt, schema, template and normalization rules
//! used by Git actions.
mod commit_message;
mod pr_content;
mod template;

pub(crate) use commit_message::{
    GeneratedCommitMessage, commit_message_prompt, commit_message_schema, format_commit_message,
    parse_custom_commit_message, sanitize_commit_message,
};
pub(crate) use pr_content::{
    GeneratedPrContent, pr_content_prompt, pr_content_schema, sanitize_pr_content,
};
pub(crate) use template::pull_request_template;

/// Keeps provider prompts bounded while preserving a visible truncation marker.
pub(crate) fn limit_section(value: &str, max_chars: usize) -> String {
    let Some((end, _)) = value.char_indices().nth(max_chars) else {
        return value.to_owned();
    };
    format!("{}\n\n[truncated]", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::limit_section;

    #[test]
    fn limit_section_marks_only_truncated_prompt_context() {
        assert_eq!(limit_section("small", 10), "small");
        assert_eq!(limit_section("abcdef", 3), "abc\n\n[truncated]");
    }
}
