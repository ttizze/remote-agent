//! Code availability is separate from capture/acceptance results. Every T3
//! component must have an explicit assessment of shared Rust and each client.
use super::Catalog;
use crate::Result;
use serde::Deserialize;
use std::{collections::BTreeMap, fmt::Write, fs, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Implementations {
    revision: String,
    sources: BTreeMap<String, String>,
    implementation: Vec<Implementation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Implementation {
    components: Vec<String>,
    phase: Phase,
    next: String,
    rust: Layer,
    mac: Layer,
    iphone: Layer,
    windows: Layer,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Deferred,
    Restore,
    Shared,
    Local,
    Extension,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Deferred => "今回の対象外",
            Self::Restore => "既存Rustの接続・描画を修復",
            Self::Shared => "共通データ・操作を追加",
            Self::Local => "UIで補完・既存機能を維持",
            Self::Extension => "Hostを含む追加機能",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Layer {
    status: Status,
    detail: String,
    evidence: Vec<String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Present,
    Partial,
    Missing,
    UiOnly,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Present => "あり",
            Self::Partial => "一部あり",
            Self::Missing => "なし",
            Self::UiOnly => "共通Rust不要",
        }
    }
}

impl Implementation {
    fn layers(&self) -> [(&str, &Layer); 4] {
        [
            ("共通Rust", &self.rust),
            ("Mac UI", &self.mac),
            ("iPhone UI", &self.iphone),
            ("Windows UI", &self.windows),
        ]
    }
}

impl Implementations {
    fn indexed<'a>(&'a self, catalog: &Catalog) -> Result<BTreeMap<&'a str, &'a Implementation>> {
        let mut indexed = BTreeMap::new();
        for assessment in &self.implementation {
            if assessment.components.is_empty() || assessment.next.trim().is_empty() {
                return Err("implementation assessment needs components and a next action".into());
            }
            for id in &assessment.components {
                if !catalog
                    .component
                    .iter()
                    .any(|component| component.id == *id)
                {
                    return Err(format!("unknown implementation component: {id}").into());
                }
                if indexed.insert(id.as_str(), assessment).is_some() {
                    return Err(format!("duplicate implementation component: {id}").into());
                }
            }
            for (name, layer) in assessment.layers() {
                self.validate_layer(name, layer)?;
            }
        }
        for component in &catalog.component {
            if !indexed.contains_key(component.id.as_str()) {
                return Err(format!("missing implementation assessment: {}", component.id).into());
            }
        }
        Ok(indexed)
    }

    fn validate_layer(&self, name: &str, layer: &Layer) -> Result<()> {
        if layer.detail.trim().is_empty() || layer.evidence.is_empty() {
            return Err(format!("{name} needs a concrete assessment and source evidence").into());
        }
        if name != "共通Rust" && layer.status == Status::UiOnly {
            return Err(format!("{name} cannot be classified as ui_only").into());
        }
        for source in &layer.evidence {
            if !self.sources.contains_key(source) {
                return Err(format!("unknown implementation evidence: {source}").into());
            }
        }
        Ok(())
    }
}

pub(super) fn write(cwd: &Path, directory: &Path, catalog: &Catalog) -> Result<()> {
    let assessments: Implementations = toml::from_str(&fs::read_to_string(
        cwd.join("apps/nsh/parity/implementation.toml"),
    )?)?;
    let indexed = assessments.indexed(catalog)?;
    for path in assessments.sources.values() {
        if !cwd.join(path).is_file() {
            return Err(format!("implementation evidence file does not exist: {path}").into());
        }
    }
    let [present, partial, missing, ui_only] = [
        Status::Present,
        Status::Partial,
        Status::Missing,
        Status::UiOnly,
    ]
    .map(|status| {
        indexed
            .values()
            .filter(|assessment| {
                assessment.phase != Phase::Deferred && assessment.rust.status == status
            })
            .count()
    });
    let mut report = String::from(
        "# T3 Codeと共通Rust・各OS UIの実装有無\n\n\
         [T3の全部品・出典](components.md)と同じ確認対象を使う。位置・サイズ・色・余白は対象外。\n\n\
         これはコード上の実装有無。画面上で全条件を実行した受け入れ結果ではない。\n\n\
         共通Rustはprotocol/core/Host側を指す。NSHやWindows UIもRustで書かれているが、UI列に分類する。\n\n\
         「あり」は対応する要素・操作がある。「一部あり」は下の詳細に具体的な不足を記す。\
         「なし」は同等の契約/部品がない。「共通Rust不要」はOSの描画・入力・表示状態で完結する。\n\n\
         共通のデータ・操作がない機能はRust側から、既にある機能は受け渡し・UIから直す。\
         一機能ずつ共通契約→受け渡し→Mac/iPhone/Windowsまで通す。\
         コピー完了・ホバー・アイコンのためだけに共通の業務状態は作らない。\n\n\
         MCP App・ゲスト共有は今回の対象外で、不足数には含めない。\n\n",
    );
    writeln!(
        report,
        "共通/UIの調査対象: [{}](https://github.com/ttizze/remote-agent/tree/{})。\
         実装変更時は評価も更新する。\n\n\
         共通Rustの不足: **{}部品群**（未実装{}・一部不足{}）。\
         実装あり{}・共通Rust不要{}・今回の対象外{}、全{}部品群。\
         同じ機能に依存する部品群もあるため、独立した機能数ではない。\n\n\
         ## 一覧\n\n\
         | 部品 | 共通Rust | Mac UI | iPhone UI | Windows UI | 次の作業 |\n\
         | --- | --- | --- | --- | --- | --- |",
        assessments.revision,
        assessments.revision,
        missing + partial,
        missing,
        partial,
        present,
        ui_only,
        indexed
            .values()
            .filter(|assessment| assessment.phase == Phase::Deferred)
            .count(),
        indexed.len(),
    )?;
    for component in &catalog.component {
        let assessment = indexed[component.id.as_str()];
        writeln!(
            report,
            "| [{}](#{}) | {} | {} | {} | {} | {} |",
            component.id,
            component.id,
            assessment.rust.status.label(),
            assessment.mac.status.label(),
            assessment.iphone.status.label(),
            assessment.windows.status.label(),
            assessment.phase.label(),
        )?;
    }
    for component in &catalog.component {
        let assessment = indexed[component.id.as_str()];
        writeln!(
            report,
            "\n## {}\n\n分類: {}。次の作業: {}\n\n\
             対象要素: {}\n\n\
             | 層 | 実装有無 | あるもの・具体的な不足 | 根拠コード |\n\
             | --- | --- | --- | --- |",
            component.id,
            assessment.phase.label(),
            assessment.next,
            component.parts.join(" / "),
        )?;
        for (name, layer) in assessment.layers() {
            let evidence = layer
                .evidence
                .iter()
                .map(|source| {
                    let path = &assessments.sources[source];
                    format!("[{source}]({})", cwd.join(path).display())
                })
                .collect::<Vec<_>>()
                .join("、");
            writeln!(
                report,
                "| {name} | {} | {} | {evidence} |",
                layer.status.label(),
                layer.detail.replace('|', "\\|"),
            )?;
        }
    }
    let path = directory.join("implementation.md");
    fs::write(&path, report)?;
    println!("Native implementation assessment: {}", path.display());
    println!(
        "Shared Rust gaps: {} components ({missing} missing, {partial} partial)",
        missing + partial
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> (Catalog, Implementations) {
        (
            toml::from_str(include_str!(
                "../../../../../apps/nsh/parity/components.toml"
            ))
            .unwrap(),
            toml::from_str(include_str!(
                "../../../../../apps/nsh/parity/implementation.toml"
            ))
            .unwrap(),
        )
    }

    fn error(catalog: &Catalog, assessments: &Implementations) -> String {
        assessments.indexed(catalog).err().unwrap().to_string()
    }

    #[test]
    fn every_t3_component_has_one_assessment_of_rust_and_all_clients() {
        let (catalog, assessments) = inputs();
        let indexed = assessments.indexed(&catalog).unwrap();
        assert_eq!(indexed.len(), catalog.component.len());
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for path in assessments.sources.values() {
            assert!(root.join(path).is_file(), "missing source evidence: {path}");
        }
    }

    #[test]
    fn omitted_duplicated_and_unknown_components_are_errors() {
        let (catalog, mut assessments) = inputs();
        let first_assessment = assessments.implementation.remove(0);
        assert!(error(&catalog, &assessments).contains("missing implementation assessment"));
        assessments.implementation.insert(0, first_assessment);
        let first = assessments.implementation[0].components[0].clone();
        assessments.implementation[0].components.clear();
        assert!(error(&catalog, &assessments).contains("needs components and a next action"));
        assessments.implementation[0].components = vec![first.clone()];
        let next = std::mem::take(&mut assessments.implementation[0].next);
        assert!(error(&catalog, &assessments).contains("needs components and a next action"));
        assessments.implementation[0].next = next;
        assessments.implementation[0].components = vec![first.clone(), first];
        assert!(error(&catalog, &assessments).contains("duplicate implementation component"));
        assessments.implementation[0].components = vec!["not-a-t3-component".into()];
        assert!(error(&catalog, &assessments).contains("unknown implementation component"));
    }

    #[test]
    fn availability_cannot_omit_evidence_or_classify_native_ui_as_unnecessary() {
        let (catalog, mut assessments) = inputs();
        assessments.implementation[0].mac.evidence = vec!["not-a-source".into()];
        assert!(error(&catalog, &assessments).contains("unknown implementation evidence"));
        assessments.implementation[0].mac.evidence.clear();
        assert!(error(&catalog, &assessments).contains("needs a concrete assessment and source"));
        assessments.implementation[0].mac.evidence = vec!["mac_rows".into()];
        let detail = std::mem::take(&mut assessments.implementation[0].mac.detail);
        assert!(error(&catalog, &assessments).contains("needs a concrete assessment and source"));
        assessments.implementation[0].mac.detail = detail;
        assessments.implementation[0].mac.status = Status::UiOnly;
        assert!(error(&catalog, &assessments).contains("cannot be classified as ui_only"));
    }
}
