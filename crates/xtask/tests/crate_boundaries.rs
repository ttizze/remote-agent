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
fn production_orchestration_host_transport_and_protocol_stay_independent_of_clients() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for crate_name in [
        "host-daemon",
        "agent-transport",
        "agent-protocol",
        "orchestration",
    ] {
        let names = reachable(
            &root.join("crates").join(crate_name).join("Cargo.toml"),
            &mut BTreeSet::new(),
        );
        if matches!(crate_name, "host-daemon" | "agent-transport") {
            assert!(names.contains("agent-protocol"));
        }
        // Shared orchestration owns contracts, storage and the async worker;
        // Tokio is now a legitimate transitive dependency of the protocol.
        let forbidden = if matches!(crate_name, "agent-protocol" | "orchestration") {
            &[
                "agent-core",
                "agent-transport",
                "host-daemon",
                "agent-ffi",
                "uniffi",
                "iroh",
                "markdown",
            ][..]
        } else {
            &["agent-core", "agent-ffi", "uniffi", "markdown"][..]
        };
        for dependency in forbidden {
            assert!(
                !names.contains(*dependency),
                "{crate_name} depends on {dependency}"
            );
        }
    }
}
