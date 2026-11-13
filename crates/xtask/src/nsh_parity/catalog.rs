//! T3's source-derived inventory. Its named parts are the presence checklist;
//! the selected states in map.toml are capture cases, not the inventory.
use crate::Result;
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    reference: Reference,
    component: Vec<Component>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    repository: String,
    revision: String,
    source_root: String,
    entry: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    id: String,
    area: String,
    source: String,
    symbol: String,
    line: usize,
    #[serde(default)]
    related_sources: Vec<String>,
    parts: Vec<String>,
}

pub fn write(cwd: &Path, target: &Path) -> Result<()> {
    let catalog: Catalog = toml::from_str(&fs::read_to_string(
        cwd.join("apps/nsh/parity/components.toml"),
    )?)?;
    let reference = &catalog.reference;
    let base = format!(
        "{}/blob/{}/{}",
        reference.repository, reference.revision, reference.source_root
    );
    let mut report = format!(
        "# T3 Code 会話部分の部品一覧\n\n基準コード: [{}]({}/tree/{})。\n\n\
         読み始め: [ChatView]({}/{}). 会話本文だけでなく、条件付きの行・装飾・操作・入力欄・会話から開くパネルを含む。\n\n\
         各項目について表示・操作要素の有無を確認する。位置、サイズ、色、余白は判定対象外。\n\n\
         この一覧は T3 のソースにある確認対象であり、Mac の実装済み・検証済み件数を表さない。\n\n",
        reference.revision, reference.repository, reference.revision, base, reference.entry,
    );
    let mut area = "";
    for component in &catalog.component {
        if area != component.area {
            area = &component.area;
            report.push_str(&format!("## {area}\n\n"));
        }
        report.push_str(&format!(
            "### {}\n\n出典: [{}]({}/{}#L{})\n\n",
            component.id, component.symbol, base, component.source, component.line,
        ));
        for part in &component.parts {
            report.push_str(&format!("- {part}\n"));
        }
        if !component.related_sources.is_empty() {
            report.push_str("\n関連する描画部品: ");
            report.push_str(
                &component
                    .related_sources
                    .iter()
                    .map(|source| {
                        format!(
                            "[{}]({base}/{source})",
                            source.rsplit('/').next().unwrap_or(source)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("、"),
            );
            report.push('\n');
        }
        report.push('\n');
    }
    let directory = target.join("nsh-parity");
    fs::create_dir_all(&directory)?;
    let path = directory.join("components.md");
    fs::write(&path, report)?;
    println!(
        "T3 source inventory: {}\n{} component groups",
        path.display(),
        catalog.component.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn source_inventory_keeps_identifiable_nonempty_parts_and_source_locations() {
        let catalog: Catalog =
            toml::from_str(include_str!("../../../../apps/nsh/parity/components.toml")).unwrap();
        assert_eq!(catalog.reference.entry, "components/ChatView.tsx");
        assert!(!catalog.component.is_empty());
        let mut ids = HashSet::new();
        for component in &catalog.component {
            assert!(
                ids.insert(&component.id),
                "duplicate component {}",
                component.id
            );
            assert!(!component.area.is_empty() && !component.symbol.is_empty());
            assert!(component.source.starts_with("components/") && component.line > 0);
            assert!(
                !component.parts.is_empty(),
                "empty component {}",
                component.id
            );
            let parts: HashSet<_> = component.parts.iter().collect();
            assert_eq!(
                parts.len(),
                component.parts.len(),
                "duplicate part in {}",
                component.id
            );
            assert!(parts.iter().all(|part| !part.trim().is_empty()));
        }
    }
}
