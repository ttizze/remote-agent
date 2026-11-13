//! Search uses the same paragraph documents as the standard TextView. Styling, link
//! destinations, images and other non-text parts never become phantom matches.
use super::{Part, parts};
use agent_core::presentation::conversation::{TextMatch, find_matches};
use serde_json::{Value, json};

pub(crate) fn documents(source: String, cwd: String, home: Option<String>) -> Vec<String> {
    let blocks = agent_core::presentation::markdown::workspace_markdown_blocks(source, cwd, home);
    fn collect(parts: &[Part], output: &mut Vec<String>) {
        for part in parts {
            match part {
                Part::Text(blocks) | Part::Code { blocks, .. } => output.push(text(blocks)),
                Part::Table { rows, .. } => {
                    for cell in rows.iter().flatten() {
                        collect(cell, output);
                    }
                }
                _ => {}
            }
        }
    }
    let mut result = Vec::new();
    collect(&parts(blocks, &[], &|_| None), &mut result);
    result
}

pub(super) fn indexed(mut parts: Vec<Part>) -> Vec<Part> {
    fn visit(parts: &mut [Part], next: &mut usize) {
        for part in parts {
            match part {
                Part::Text(blocks) | Part::Code { blocks, .. } => {
                    if let Some(first) = blocks.first_mut() {
                        first["findDocument"] = json!(*next);
                    }
                    *next += 1;
                }
                Part::Table { rows, .. } => {
                    for cell in rows.iter_mut().flatten() {
                        visit(cell, next);
                    }
                }
                _ => {}
            }
        }
    }
    visit(&mut parts, &mut 0);
    parts
}

pub(super) fn document(blocks: &[Value]) -> usize {
    blocks
        .first()
        .and_then(|block| block["findDocument"].as_u64())
        .unwrap_or_default() as usize
}

fn text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .map(|block| {
            let mut text = block["marker"]
                .as_str()
                .map(|marker| format!("{marker} "))
                .unwrap_or_default();
            for run in block["runs"].as_array().into_iter().flatten() {
                text.push_str(run["text"].as_str().unwrap_or_default());
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn highlights(
    ranges: &[TextMatch],
    active: Option<TextMatch>,
    start: usize,
    length: usize,
) -> Value {
    json!(ranges.iter().filter_map(|range| {
        let from = range.start.max(start);
        let to = range.end.min(start + length);
            (from < to).then(|| json!({ "start": from - start, "end": to - start, "active": Some(*range) == active, "reveal": Some(*range) == active && from == range.start }))
    }).collect::<Vec<_>>())
}

pub(super) fn decorate(
    mut blocks: Vec<Value>,
    query: &str,
    active: Option<TextMatch>,
) -> Vec<Value> {
    if query.trim().is_empty() {
        return blocks;
    }
    let ranges = find_matches(&text(&blocks), query);
    let mut offset = 0;
    for block in &mut blocks {
        if let Some(marker) = block["marker"].as_str() {
            let length = marker.len() + 1;
            block["markerHighlights"] = highlights(&ranges, active, offset, length);
            offset += length;
        }
        for run in block["runs"].as_array_mut().into_iter().flatten() {
            let length = run["text"].as_str().unwrap_or_default().len();
            run["highlights"] = highlights(&ranges, active, offset, length);
            offset += length;
        }
        offset += 1; // the paragraph separator TextView draws
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_sees_rendered_labels_and_emphasis_across_runs_without_urls() {
        let docs = documents(
            "[Open](https://example.com) **hello** world".into(),
            "/workspace".into(),
            None,
        );
        assert_eq!(docs, ["Open hello world"]);
        assert!(find_matches(&docs[0], "example.com").is_empty());
        assert_eq!(
            find_matches(&docs[0], "hello world"),
            [TextMatch { start: 5, end: 16 }]
        );
        let blocks = vec![
            json!({"marker": null, "runs": [{"text":"Open "}, {"text":"hello", "strong":true}, {"text":" world"}]}),
        ];
        let decorated = decorate(blocks, "hello world", Some(TextMatch { start: 5, end: 16 }));
        assert_eq!(
            decorated[0]["runs"][1]["highlights"],
            json!([{"start":0,"end":5,"active":true,"reveal":true}])
        );
        assert_eq!(
            decorated[0]["runs"][2]["highlights"],
            json!([{"start":0,"end":6,"active":true,"reveal":false}])
        );
    }
}
