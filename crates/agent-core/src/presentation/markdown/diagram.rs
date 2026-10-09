use super::{Node, parse};
use mermaid_rs_renderer::{LayoutConfig, Theme, compute_layout, parse_mermaid, render_svg};

#[derive(Clone)]
pub struct MarkdownDiagram {
    pub source: String,
    pub svg: String,
    pub width: f32,
    pub height: f32,
}

/// Render complete, bounded Mermaid blocks; unsupported and streaming blocks stay selectable code.
pub fn markdown_diagram(fenced_source: &str) -> Option<MarkdownDiagram> {
    if fenced_source.len() > 16 * 1024 {
        return None;
    }
    let root = parse(fenced_source);
    let children = root.children()?;
    let [Node::Code(code)] = children.as_slice() else {
        return None;
    };
    if code.lang.as_deref() != Some("mermaid") {
        return None;
    }
    let mut lines = fenced_source.lines();
    let first = lines.next()?.trim_start();
    let fence = first.chars().next()?;
    if !matches!(fence, '`' | '~') {
        return None;
    }
    let opening = first.chars().take_while(|c| *c == fence).count();
    let last = lines.last()?.trim();
    if opening < 3 || last.chars().count() < opening || !last.chars().all(|c| c == fence) {
        return None;
    }
    let parsed = parse_mermaid(&code.value).ok()?;
    if parsed.graph.nodes.len() > 80 || parsed.graph.edges.len() > 160 {
        return None;
    }
    let theme = Theme::modern();
    let config = LayoutConfig::default();
    let layout = compute_layout(&parsed.graph, &theme, &config);
    if !layout.width.is_finite()
        || !layout.height.is_finite()
        || layout.width <= 0.0
        || layout.height <= 0.0
    {
        return None;
    }
    Some(MarkdownDiagram {
        source: code.value.clone(),
        svg: render_svg(&layout, &theme, &config),
        width: layout.width,
        height: layout.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_valid_diagrams_replace_code() {
        let diagram =
            markdown_diagram("```mermaid\nflowchart LR\nA[開始] --> B[完了]\n```").unwrap();
        assert!(diagram.svg.contains("<svg"));
        assert!(diagram.source.contains("開始"));
        assert!(markdown_diagram("```mermaid\nflowchart LR\nA --> B").is_none());
        assert!(markdown_diagram("```rust\nflowchart LR\nA --> B\n```").is_none());
        assert!(markdown_diagram("```mermaid\ninvalidDiagram\n```").is_none());
        assert!(
            markdown_diagram(&format!(
                "```mermaid\nflowchart LR\n{}\n```",
                (0..81).map(|n| format!("N{n};")).collect::<String>()
            ))
            .is_none()
        );
    }
}
