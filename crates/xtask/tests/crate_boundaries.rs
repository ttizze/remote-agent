//! Preserve production dependency boundaries independently of dev dependencies.
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn reachable(
    manifest: &Path,
    requested: BTreeSet<String>,
    visited: &mut BTreeSet<(PathBuf, BTreeSet<String>)>,
) -> BTreeSet<String> {
    let manifest = manifest.canonicalize().unwrap();
    if !visited.insert((manifest.clone(), requested.clone())) {
        return BTreeSet::new();
    }
    let data: toml::Value = toml::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    let definitions = data.get("features").and_then(toml::Value::as_table);
    let mut features = requested;
    loop {
        let old = features.len();
        for feature in features.clone() {
            if let Some(members) = definitions
                .and_then(|f| f.get(&feature))
                .and_then(toml::Value::as_array)
            {
                features.extend(
                    members
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned),
                );
            }
        }
        if features.len() == old {
            break;
        }
    }
    let mut sections = vec![&data];
    if let Some(targets) = data.get("target").and_then(toml::Value::as_table) {
        sections.extend(targets.values());
    }
    let mut names = BTreeSet::new();
    for section in sections {
        for kind in ["dependencies", "build-dependencies"] {
            if let Some(dependencies) = section.get(kind).and_then(toml::Value::as_table) {
                for (name, config) in dependencies {
                    let enabled = features.contains(name)
                        || features.contains(&format!("dep:{name}"))
                        || features.iter().any(|f| f.starts_with(&format!("{name}/")));
                    if config.get("optional").and_then(toml::Value::as_bool) == Some(true)
                        && !enabled
                    {
                        continue;
                    }
                    names.insert(
                        config
                            .get("package")
                            .and_then(toml::Value::as_str)
                            .unwrap_or(name)
                            .to_owned(),
                    );
                    if let Some(path) = config.get("path").and_then(toml::Value::as_str) {
                        let mut child = BTreeSet::new();
                        if config
                            .get("default-features")
                            .and_then(toml::Value::as_bool)
                            != Some(false)
                        {
                            child.insert("default".to_owned());
                        }
                        if let Some(explicit) =
                            config.get("features").and_then(toml::Value::as_array)
                        {
                            child.extend(
                                explicit
                                    .iter()
                                    .filter_map(toml::Value::as_str)
                                    .map(str::to_owned),
                            );
                        }
                        for f in &features {
                            if let Some(value) = f
                                .strip_prefix(&format!("{name}/"))
                                .or_else(|| f.strip_prefix(&format!("{name}?/")))
                            {
                                child.insert(value.to_owned());
                            }
                        }
                        names.extend(reachable(
                            &manifest.parent().unwrap().join(path).join("Cargo.toml"),
                            child,
                            visited,
                        ));
                    }
                }
            }
        }
    }
    names
}

fn production_dependencies(crate_name: &str, features: &[&str]) -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    reachable(
        &root.join("crates").join(crate_name).join("Cargo.toml"),
        features
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
        &mut BTreeSet::new(),
    )
}

#[test]
fn production_domain_host_transport_and_protocol_stay_independent_of_clients() {
    for crate_name in [
        "host-daemon",
        "agent-transport",
        "agent-protocol",
        "agent-domain",
    ] {
        let names = production_dependencies(crate_name, &["default"]);
        if matches!(crate_name, "host-daemon" | "agent-transport") {
            assert!(names.contains("agent-protocol"));
        }
        let forbidden = if matches!(crate_name, "agent-protocol" | "agent-domain") {
            &[
                "agent-core",
                "agent-transport",
                "host-daemon",
                "agent-runtime",
                "agent-providers",
                "agent-ffi",
                "uniffi",
                "iroh",
                "markdown",
                "tokio",
                "rusqlite",
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

/// The domain is pure: commands, facts and folds with no I/O, so every
/// production dependency is a data or text crate.
#[test]
fn domain_has_no_io_dependencies() {
    let pure = BTreeSet::from(
        [
            "chrono",
            "percent-encoding",
            "regex",
            "serde",
            "serde_json",
            "thiserror",
            "url",
        ]
        .map(str::to_owned),
    );
    let names = production_dependencies("agent-domain", &["default"]);
    let impure: Vec<_> = names.difference(&pure).collect();
    assert!(impure.is_empty(), "agent-domain depends on {impure:?}");
}

#[test]
fn runtime_and_host_persistence_stay_out_of_clients() {
    assert!(production_dependencies("host-daemon", &["default"]).contains("rusqlite"));
    for (client, features) in [
        ("agent-core", &["default"][..]),
        ("agent-core", &["default", "bindings"][..]),
        ("agent-ffi", &["default"][..]),
    ] {
        let dependencies = production_dependencies(client, features);
        for host_only in ["agent-runtime", "agent-providers", "rusqlite"] {
            assert!(
                !dependencies.contains(host_only),
                "{client} {features:?} pulls {host_only} into the client"
            );
        }
    }
}
