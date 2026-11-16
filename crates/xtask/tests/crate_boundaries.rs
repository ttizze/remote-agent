//! Preserve production dependency boundaries independently of dev dependencies.
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn reachable(manifest: &Path, visited: &mut BTreeSet<PathBuf>) -> BTreeSet<String> {
    let manifest = manifest.canonicalize().unwrap();
    if !visited.insert(manifest.clone()) {
        return BTreeSet::new();
    }
    let data: toml::Value = toml::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    let mut sections = vec![&data];
    if let Some(targets) = data.get("target").and_then(toml::Value::as_table) {
        sections.extend(targets.values());
    }
    let mut names = BTreeSet::new();
    for section in sections {
        for kind in ["dependencies", "build-dependencies"] {
            if let Some(dependencies) = section.get(kind).and_then(toml::Value::as_table) {
                for (name, config) in dependencies {
                    names.insert(
                        config
                            .get("package")
                            .and_then(toml::Value::as_str)
                            .unwrap_or(name)
                            .to_owned(),
                    );
                    if let Some(path) = config.get("path").and_then(toml::Value::as_str) {
                        names.extend(reachable(
                            &manifest.parent().unwrap().join(path).join("Cargo.toml"),
                            visited,
                        ));
                    }
                }
            }
        }
    }
    names
}

#[test]
fn host_binaries_and_protocol_stay_independent_of_clients() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for crate_name in [
        "host-daemon",
        "host-fixture",
        "agent-transport",
        "agent-protocol",
    ] {
        let names = reachable(
            &root.join("crates").join(crate_name).join("Cargo.toml"),
            &mut BTreeSet::new(),
        );
        if crate_name != "agent-protocol" {
            assert!(names.contains("agent-protocol"));
        }
        let forbidden = if crate_name == "agent-protocol" {
            &[
                "agent-core",
                "agent-transport",
                "host-daemon",
                "agent-ffi",
                "uniffi",
                "tokio",
                "iroh",
            ][..]
        } else {
            // The pure `markdown` parser is shared: like T3's server, the Host
            // reads Markdown structure to place generated-image embeds.
            &["agent-core", "agent-ffi", "uniffi"][..]
        };
        for dependency in forbidden {
            assert!(
                !names.contains(*dependency),
                "{crate_name} depends on {dependency}"
            );
        }
    }
}

#[test]
fn clients_reach_the_host_only_through_the_transport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for manifest in [
        "apps/nsh/Cargo.toml",
        "crates/agent-core/Cargo.toml",
        "crates/agent-ffi/Cargo.toml",
        "crates/agent-cli/Cargo.toml",
    ] {
        let names = reachable(&root.join(manifest), &mut BTreeSet::new());
        assert!(
            !names.contains("host-daemon"),
            "{manifest} depends on host-daemon"
        );
    }
}

#[test]
fn binding_generator_stays_independent_of_the_application() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let names = reachable(
        &root.join("crates/agent-bindgen/Cargo.toml"),
        &mut BTreeSet::new(),
    );
    assert!(names.contains("uniffi_bindgen"));
    for dependency in [
        "uniffi",
        "agent-core",
        "agent-ffi",
        "agent-transport",
        "host-daemon",
    ] {
        assert!(
            !names.contains(dependency),
            "bindgen depends on {dependency}"
        );
    }
}
