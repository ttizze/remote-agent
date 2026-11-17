use std::sync::LazyLock;

/// Only a complete, single-line shell fence can offer the native Run action.
pub(super) fn command(language: Option<&str>, code: &str, fenced: &str) -> Option<String> {
    if !matches!(
        language?,
        "sh" | "bash" | "zsh" | "fish" | "shell" | "powershell" | "pwsh"
    ) {
        return None;
    }
    let mut lines = fenced.lines();
    let first = lines.next()?.trim_start();
    let marker = first.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let width = first.chars().take_while(|c| *c == marker).count();
    let last = lines
        .last()?
        .trim_start_matches([' ', '\t', '>'])
        .trim_end();
    if last.chars().count() < width || !last.chars().all(|c| c == marker) {
        return None;
    }
    static INVISIBLE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[\p{Cc}\p{Cf}]").unwrap());
    let command = code.trim();
    if command.is_empty() || command.ends_with('\\') || INVISIBLE.is_match(code) {
        return None;
    }
    Some(command.into())
}

#[cfg(test)]
mod tests {
    use crate::presentation::markdown::{MarkdownBlock, markdown_blocks};
    fn command(source: &str) -> Option<String> {
        markdown_blocks(source.into())
            .into_iter()
            .find_map(|block| match block {
                MarkdownBlock::Paragraph { style, .. } => style.shell_command,
                _ => None,
            })
    }
    #[test]
    fn only_complete_literal_shell_fences_offer_run() {
        for language in ["sh", "bash", "zsh", "fish", "shell", "powershell", "pwsh"] {
            assert_eq!(
                command(&format!("```{language}\necho '日本語 😀'\n```")),
                Some("echo '日本語 😀'".into())
            );
        }
        for source in [
            "```sh\necho hi",
            "```sh\necho hi\n``",
            "```sh\n``",
            "```SH\necho hi\n```",
            "```python\necho hi\n```",
            "```sh\n\n```",
            "```sh\necho hi \\\n```",
            "```sh\necho hi\necho again\n```",
            "```sh\necho\thi\n```",
            "```sh\necho \u{202e}hi\n```",
            "```sh\necho \u{200b}hi\n```",
        ] {
            assert_eq!(command(source), None, "{source:?}");
        }
        assert_eq!(command("~~~sh\nprintf ok\n~~~~"), Some("printf ok".into()));
        assert_eq!(command("> ```sh\n> echo ok\n> ```"), Some("echo ok".into()));
    }
}
