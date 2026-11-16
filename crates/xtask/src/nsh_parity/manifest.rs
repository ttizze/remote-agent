//! Provenance for captured pixels. Recomparison keeps this manifest: changing
//! selectors must not make an old screenshot look like a new client build.
use crate::Result;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, process::Command};

pub fn write(cwd: &Path, records: &Path, app: &Path, fixture: &Path, map: &Path) -> Result<()> {
    let app_info = app
        .parent()
        .and_then(Path::parent)
        .ok_or("app has no Contents directory")?
        .join("Info.plist");
    let manifest = json!({
        "candidate": {
            "platform": "macos", "executable": app, "sha256": checksum(app)?,
            "buildRevision": plist(&app_info, "BEXBuildRevision")?,
            "checkoutRevision": git(cwd, &["rev-parse", "HEAD"])?,
            "dirty": !git(cwd, &["status", "--porcelain"])?.is_empty(),
        },
        "fixture": {"executable": fixture, "sha256": checksum(fixture)?},
        "mapSha256": checksum(map)?,
    });
    fs::write(
        records.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::copy(map, records.join("captured-map.toml"))?;
    Ok(())
}

pub fn reference(records: &Path, app: &str, root: &Path, fixture: &Path) -> Result<()> {
    let info = Path::new(app).join("Contents/Info.plist");
    let candidate: serde_json::Value =
        serde_json::from_slice(&fs::read(records.join("manifest.json"))?)?;
    let fixture_sha = checksum(fixture)?;
    if candidate["fixture"]["sha256"].as_str() != Some(&fixture_sha) {
        return Err("the fixture binary changed; capture both clients again instead of rerendering T3 alone".into());
    }
    fs::write(
        records.join("reference-manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "app": app, "version": plist(&info, "CFBundleShortVersionString")?,
            "build": plist(&info, "CFBundleVersion")?, "dependenciesCheckout": root,
            "dependenciesRevision": git(root, &["rev-parse", "HEAD"])?, "fixtureSha256": fixture_sha,
        }))?,
    )?;
    Ok(())
}

fn git(cwd: &Path, arguments: &[&str]) -> Result<String> {
    let result = Command::new("git")
        .current_dir(cwd)
        .args(arguments)
        .output()?;
    if !result.status.success() {
        return Err(format!(
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(result.stdout)?.trim().into())
}
fn checksum(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn plist(path: &Path, key: &str) -> Result<String> {
    let plist = plist::Value::from_file(path)?;
    plist
        .as_dictionary()
        .and_then(|values| values.get(key))
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
        .ok_or_else(|| format!("{} has no string {key}", path.display()).into())
}
