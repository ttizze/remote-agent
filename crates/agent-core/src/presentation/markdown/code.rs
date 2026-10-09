use super::MarkdownRun;
use std::sync::LazyLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{Color, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEMES: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

pub(super) fn fence_filename(metadata: Option<&str>) -> Option<String> {
    let value = metadata?.trim();
    let value = value
        .strip_prefix("title=")
        .or_else(|| value.strip_prefix("filename="))
        .unwrap_or(value);
    let value = if let Some(quote) = value.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        value[1..].split(quote).next()?
    } else {
        value.split_whitespace().next()?
    };
    (value.contains('/') || value.contains('.') || value.contains('\\')).then(|| value.to_owned())
}

fn rgb(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

pub(super) fn highlight(text: &str, language: Option<&str>) -> Vec<MarkdownRun> {
    let fallback = || {
        vec![MarkdownRun {
            text: text.into(),
            ..Default::default()
        }]
    };
    let Some(language) = language else {
        return fallback();
    };
    let language = match language {
        "typescript" | "ts" | "tsx" | "jsx" => "js",
        "shell" | "bash" | "zsh" => "sh",
        "kotlin" => "java",
        other => other,
    };
    let Some(syntax) = SYNTAXES.find_syntax_by_token(language) else {
        return fallback();
    };
    // Bound synchronous work while streaming very large tool output.
    if text.len() > 128 * 1024 {
        return fallback();
    }
    let mut dark = HighlightLines::new(syntax, &THEMES.themes["base16-ocean.dark"]);
    let mut light = HighlightLines::new(syntax, &THEMES.themes["InspiredGitHub"]);
    let mut runs = Vec::new();
    for line in LinesWithEndings::from(text) {
        let Ok(dark) = dark.highlight_line(line, &SYNTAXES) else {
            return fallback();
        };
        let Ok(light) = light.highlight_line(line, &SYNTAXES) else {
            return fallback();
        };
        if dark.len() != light.len() || dark.iter().zip(&light).any(|((_, a), (_, b))| a != b) {
            return fallback();
        }
        for ((dark, text), (light, _)) in dark.into_iter().zip(light) {
            runs.push(MarkdownRun {
                text: text.into(),
                dark_color: Some(rgb(dark.foreground)),
                light_color: Some(rgb(light.foreground)),
                ..Default::default()
            });
        }
    }
    if runs.is_empty() { fallback() } else { runs }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fenced_metadata_and_highlighted_text_survive() {
        assert_eq!(
            fence_filename(Some("title=\"src/hello world.rs\"")),
            Some("src/hello world.rs".into())
        );
        assert_eq!(
            fence_filename(Some("filename=main.swift")),
            Some("main.swift".into())
        );
        assert_eq!(fence_filename(Some("linenos")), None);
        let text = "fn main() {\n    println!(\"こんにちは\");\n}";
        let runs = highlight(text, Some("rust"));
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            text
        );
        assert!(
            runs.iter()
                .filter_map(|run| run.dark_color)
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1
        );
        let unknown = highlight(text, Some("unknown-grammar"));
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].text, text);
        assert_eq!(unknown[0].dark_color, None);
    }
    proptest::proptest! {
        #[test]
        fn highlighting_never_rewrites_unicode_code(text in "[^\\x00]{0,128}") {
            let runs = highlight(&text, Some("rs"));
            proptest::prop_assert_eq!(runs.iter().map(|run| run.text.as_str()).collect::<String>(), text);
        }
    }
}
