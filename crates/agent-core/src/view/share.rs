//! Deterministic normalization for content received through a platform share.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ShareContent {
    pub text: String,
    pub urls: Vec<String>,
}

/// Produces the composer text for one incoming share, preserving order while
/// removing blank duplicate lines and duplicate URLs.
pub fn compose(content: &ShareContent) -> String {
    let mut lines = Vec::new();
    let mut add = |line: &str| {
        let line = line.trim();
        if !line.is_empty() && !lines.iter().any(|known| known == line) {
            lines.push(line.to_owned());
        }
    };
    for line in content.text.lines() {
        add(line);
    }
    for url in &content.urls {
        add(url);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_text_is_trimmed_deduplicated_and_ordered() {
        assert_eq!(
            compose(&ShareContent {
                text: "  explain this\n\nhttps://example.test  ".into(),
                urls: vec!["https://example.test".into(), "https://other.test".into()],
            }),
            "explain this\nhttps://example.test\nhttps://other.test"
        );
    }

    #[test]
    fn empty_share_does_not_create_composer_text() {
        assert!(compose(&ShareContent::default()).is_empty());
    }
}
