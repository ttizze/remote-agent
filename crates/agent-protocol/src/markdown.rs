//! Pure Markdown source operations shared by native rendering and Host messages.
use ::markdown::{ParseOptions, mdast::Node};
use std::collections::HashMap;

mod generated;
mod windows_paths;
pub use generated::append_generated_image_embeds;

pub fn parse(source: &str) -> Node {
    // GFM has no syntax errors; only the disabled MDX extensions can fail.
    let mut root = ::markdown::to_mdast(source, &ParseOptions::gfm()).expect("GFM is infallible");
    windows_paths::preserve_destinations(&mut root, source);
    root
}

pub fn definitions(node: &Node) -> HashMap<&str, &str> {
    fn collect<'a>(node: &'a Node, result: &mut HashMap<&'a str, &'a str>) {
        if let Node::Definition(def) = node {
            result.entry(&def.identifier).or_insert(&def.url);
        }
        for child in node.children().into_iter().flatten() {
            collect(child, result);
        }
    }
    let mut result = HashMap::new();
    collect(node, &mut result);
    result
}

pub fn image<'a>(
    node: &'a Node,
    definition: impl Fn(&str) -> Option<&'a str>,
) -> Option<(&'a str, &'a str)> {
    match node {
        Node::Image(image) => Some((&image.url, &image.alt)),
        Node::ImageReference(image) => {
            definition(&image.identifier).map(|url| (url, image.alt.as_str()))
        }
        _ => None,
    }
}

/// Every image in reading order, including references.
pub fn images(source: &str) -> Vec<String> {
    fn visit(node: &Node, definitions: &HashMap<&str, &str>, images: &mut Vec<String>) {
        if let Some((url, _)) = image(node, |id| definitions.get(id).copied()) {
            images.push(url.to_owned());
        }
        for child in node.children().into_iter().flatten() {
            visit(child, definitions, images);
        }
    }
    let mut images = Vec::new();
    // Inline and reference images both start with `![`; most text needs no parse.
    if source.contains("![") {
        let root = parse(source);
        visit(&root, &definitions(&root), &mut images);
    }
    images
}
