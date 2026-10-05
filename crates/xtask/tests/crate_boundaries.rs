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
            BTreeSet::from(["default".to_owned()]),
            &mut BTreeSet::new(),
        );
        if matches!(crate_name, "host-daemon" | "agent-transport") {
            assert!(names.contains("agent-protocol"));
        }
        let forbidden = if matches!(crate_name, "agent-protocol" | "orchestration") {
            &[
                "agent-core",
                "agent-transport",
                "host-daemon",
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

#[test]
fn runtime_feature_is_host_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let host = reachable(
        &root.join("crates/host-daemon/Cargo.toml"),
        BTreeSet::from(["default".into()]),
        &mut BTreeSet::new(),
    );
    assert!(host.contains("rusqlite"));
    for client in ["agent-core", "agent-ffi"] {
        let dependencies = reachable(
            &root.join(format!("crates/{client}/Cargo.toml")),
            BTreeSet::from(["default".into()]),
            &mut BTreeSet::new(),
        );
        assert!(
            !dependencies.contains("rusqlite"),
            "{client} pulls Host persistence into the client"
        );
    }
}
