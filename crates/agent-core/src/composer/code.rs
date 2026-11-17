use markdown::mdast::Node;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerCodeFence {
    pub start: u64,
    pub info: String,
    pub language: String,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerCodeLanguage {
    pub id: String,
    pub label: String,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn composer_code_languages() -> Vec<ComposerCodeLanguage> {
    [
        ("", "Plain text"),
        ("asm", "Assembly"),
        ("bash", "Bash"),
        ("c", "C"),
        ("cpp", "C++"),
        ("csharp", "C#"),
        ("css", "CSS"),
        ("dart", "Dart"),
        ("diff", "Diff"),
        ("dockerfile", "Dockerfile"),
        ("elixir", "Elixir"),
        ("go", "Go"),
        ("graphql", "GraphQL"),
        ("haskell", "Haskell"),
        ("html", "HTML"),
        ("java", "Java"),
        ("javascript", "JavaScript"),
        ("json", "JSON"),
        ("jsx", "JSX"),
        ("kotlin", "Kotlin"),
        ("lua", "Lua"),
        ("markdown", "Markdown"),
        ("php", "PHP"),
        ("powershell", "PowerShell"),
        ("python", "Python"),
        ("r", "R"),
        ("ruby", "Ruby"),
        ("rust", "Rust"),
        ("scala", "Scala"),
        ("scss", "SCSS"),
        ("sql", "SQL"),
        ("svelte", "Svelte"),
        ("swift", "Swift"),
        ("toml", "TOML"),
        ("tsx", "TSX"),
        ("typescript", "TypeScript"),
        ("vue", "Vue"),
        ("xml", "XML"),
        ("yaml", "YAML"),
        ("zig", "Zig"),
    ]
    .into_iter()
    .map(|(id, label)| ComposerCodeLanguage {
        id: id.into(),
        label: label.into(),
    })
    .collect()
}

fn canonical(language: &str) -> &str {
    match language {
        "assembly" | "s" => "asm",
        "cs" => "csharp",
        "c++" => "cpp",
        "js" => "javascript",
        "md" => "markdown",
        "plaintext" | "text" | "txt" => "",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "sh" | "shell" => "bash",
        "ts" => "typescript",
        "yml" => "yaml",
        other => other,
    }
}

fn fence_width(source: &str) -> usize {
    let Some(marker @ ('`' | '~')) = source.chars().next() else {
        return 0;
    };
    source.chars().take_while(|c| *c == marker).count()
}

/// AST positions keep literal fences inside prose and indented code out of the controls.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn composer_code_fences(text: &str) -> Vec<ComposerCodeFence> {
    fn collect(node: &Node, text: &str, fences: &mut Vec<ComposerCodeFence>) {
        if let Node::Code(code) = node {
            let start = code
                .position
                .as_ref()
                .expect("parsed code position")
                .start
                .offset;
            let first = text[start..].lines().next().unwrap_or_default();
            let width = fence_width(first);
            if width >= 3 {
                let info = first[width..].trim_end_matches('\r').to_owned();
                let language =
                    canonical(info.split_whitespace().next().unwrap_or_default()).to_owned();
                fences.push(ComposerCodeFence {
                    start: start as u64,
                    info,
                    language,
                });
            }
        }
        if let Some(children) = node.children() {
            for child in children {
                collect(child, text, fences);
            }
        }
    }
    let mut fences = Vec::new();
    if text.contains("```") || text.contains("~~~") {
        collect(&agent_protocol::markdown::parse(text), text, &mut fences);
    }
    fences
}

/// Change only the opening fence's language. A delayed menu cannot overwrite a newer header.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn composer_code_language(
    text: &str,
    fence: ComposerCodeFence,
    language: &str,
) -> Option<String> {
    if !composer_code_languages()
        .iter()
        .any(|choice| choice.id == language)
        || !composer_code_fences(text).contains(&fence)
    {
        return None;
    }
    let leading = fence.info.len() - fence.info.trim_start().len();
    let token_end = fence.info[leading..]
        .find(char::is_whitespace)
        .map_or(fence.info.len(), |offset| leading + offset);
    let rest = &fence.info[token_end..];
    let language = if language.is_empty() && !rest.trim().is_empty() {
        "text"
    } else {
        language
    };
    let start = fence.start as usize;
    let width = fence_width(&text[start..]);
    let mut result = text.to_owned();
    result.replace_range(start + width + leading..start + width + token_end, language);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_only_the_selected_header_and_preserves_authored_source() {
        let text = "日本語 😀\r\n\r\n```   js title=\"a b\"\r\nconst x = 1;\r\n```\r\n\n~~~py\nprint('ok')\n~~~";
        let fences = composer_code_fences(text);
        assert_eq!(fences.len(), 2);
        assert_eq!(fences[0].language, "javascript");
        assert_eq!(fences[1].language, "python");
        let changed = composer_code_language(text, fences[0].clone(), "rust").unwrap();
        assert_eq!(changed, text.replacen("js title", "rust title", 1));
        assert_eq!(
            composer_code_language(text, fences[0].clone(), "").unwrap(),
            text.replacen("js title", "text title", 1)
        );
        assert_eq!(
            composer_code_language(&changed, fences[0].clone(), "go"),
            None
        );
        assert_eq!(
            composer_code_language(text, fences[0].clone(), "\nbad"),
            None
        );
        assert_eq!(
            composer_code_language(text, fences[1].clone(), "").unwrap(),
            text.replace("~~~py", "~~~")
        );
    }

    #[test]
    fn offers_unclosed_nested_fences_but_never_literal_or_indented_code() {
        assert!(composer_code_fences("A ```js inline``` fence\n\n    echo hi").is_empty());
        let text = "> ```\n> echo ok\n> ```\n\n```swift\nlet x = 1";
        let fences = composer_code_fences(text);
        assert_eq!(fences.len(), 2);
        assert_eq!(
            composer_code_language(text, fences[0].clone(), "bash").unwrap(),
            text.replacen("> ```", "> ```bash", 1)
        );
        assert_eq!(
            composer_code_language(text, fences[1].clone(), "python").unwrap(),
            text.replace("```swift", "```python")
        );
    }

    #[test]
    fn treats_the_other_fence_character_as_part_of_the_language() {
        for (source, expected) in [
            (
                "```~legacy title=file\nbody\n```",
                "```swift title=file\nbody\n```",
            ),
            (
                "~~~`legacy title=file\nbody\n~~~",
                "~~~swift title=file\nbody\n~~~",
            ),
        ] {
            let fence = composer_code_fences(source).remove(0);
            assert_eq!(
                composer_code_language(source, fence, "swift").unwrap(),
                expected
            );
        }
    }
}
