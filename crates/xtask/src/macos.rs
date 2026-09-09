use crate::command;
use std::{fs, path::Path};
use tokio::process::Command;
use xtask::{Result, repository_root};

async fn signing_identity() -> Result<String> {
    if let Some(identity) =
        std::env::var_os("BEX_CODE_SIGN_IDENTITY").filter(|identity| !identity.is_empty())
    {
        let identity = identity
            .into_string()
            .map_err(|_| "signing identity must be UTF-8")?;
        if identity == "-" {
            return Err("Bex requires certificate signing: ad-hoc signing loses Keychain authorization on every update.".into());
        }
        return Ok(identity);
    }
    let output = command::output(Command::new("/usr/bin/security").args([
        "find-identity",
        "-v",
        "-p",
        "codesigning",
    ]))
    .await?;
    let output = std::str::from_utf8(&output)?;
    let mut identities = output
        .lines()
        .filter(|line| {
            line.contains("\"Apple Development:") || line.contains("\"Developer ID Application:")
        })
        .filter_map(|line| line.split_whitespace().nth(1));
    match (identities.next(), identities.next()) {
        (Some(identity), None) => Ok(identity.to_owned()),
        _ => Err("Set BEX_CODE_SIGN_IDENTITY to one Apple Development or Developer ID Application certificate identity; no unique identity was found.".into()),
    }
}

async fn sign(path: &Path, identity: &str, identifier: Option<&str>) -> Result<()> {
    let mut command = Command::new("/usr/bin/codesign");
    command.args(["--force", "--sign", identity, "--timestamp=none"]);
    if let Some(identifier) = identifier {
        command.args(["--identifier", identifier]);
    }
    command::run(command.arg(path)).await
}

async fn verify(path: &Path) -> Result<()> {
    command::run(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(path),
    )
    .await
}

pub async fn build_host() -> Result<()> {
    if std::env::consts::OS != "macos" {
        return Err("Host signing requires macOS".into());
    }
    let identity = signing_identity().await?;
    let target = command::target_directory().await?;
    command::run(command::cargo().args([
        "build",
        "--locked",
        "--package",
        "host-daemon",
        "--release",
    ]))
    .await?;
    let host = target.join("release/host-daemon");
    sign(&host, &identity, Some("app.bex.host")).await?;
    verify(&host).await?;
    println!("{}", host.display());
    Ok(())
}

pub async fn build_desktop() -> Result<()> {
    if (std::env::consts::OS, std::env::consts::ARCH) != ("macos", "aarch64") {
        return Err("The GPUI Mac bundle currently targets Apple Silicon.".into());
    }
    let identity = signing_identity().await?;
    let target = command::target_directory().await?;
    command::run(Command::new("npm").args([
        "--prefix",
        "apps/desktop/web",
        "ci",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
    ]))
    .await?;
    command::run(command::cargo().args([
        "build",
        "--locked",
        "--package",
        "host-daemon",
        "--package",
        "bex-desktop",
        "--release",
    ]))
    .await?;
    fs::create_dir_all(&target)?;
    let staging = tempfile::Builder::new()
        .prefix(".Bex-build.")
        .tempdir_in(&target)?;
    let bundle = staging.path().join("Bex.app");
    let executables = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");
    fs::create_dir_all(&executables)?;
    fs::create_dir_all(&resources)?;
    fs::copy("apps/desktop/assets/icon.icns", resources.join("Bex.icns"))?;
    fs::copy(target.join("release/bex-desktop"), executables.join("Bex"))?;
    fs::copy(
        target.join("release/host-daemon"),
        executables.join("host-daemon"),
    )?;
    let terminal = resources.join("terminal");
    fs::create_dir(&terminal)?;
    let modules = repository_root().join("apps/desktop/web/node_modules/@xterm");
    for (source, name) in [
        ("xterm/lib/xterm.js", "xterm.js"),
        ("xterm/css/xterm.css", "xterm.css"),
        ("addon-fit/lib/addon-fit.js", "addon-fit.js"),
        ("xterm/LICENSE", "LICENSE-xterm"),
        ("addon-fit/LICENSE", "LICENSE-addon-fit"),
    ] {
        fs::copy(modules.join(source), terminal.join(name))?;
    }
    let dictation = resources.join("Bex Dictation.app");
    fs::create_dir_all(dictation.join("Contents/MacOS"))?;
    command::run(
        Command::new("xcrun")
            .args(["swiftc", "-O", "apps/desktop/macos/Dictation.swift", "-o"])
            .arg(dictation.join("Contents/MacOS/Dictation")),
    )
    .await?;
    plist::Value::from(
        dictionary(&[
            ("CFBundleIdentifier", "app.bex.dictation"),
            ("CFBundleName", "Bex"),
            ("CFBundleExecutable", "Dictation"),
            ("CFBundlePackageType", "APPL"),
            (
                "NSMicrophoneUsageDescription",
                "音声を録音し、接続先のHostを通じて文字起こしして入力します。",
            ),
        ])
        .into_iter()
        .chain([("LSUIElement".into(), plist::Value::Boolean(true))])
        .collect::<plist::Dictionary>(),
    )
    .to_file_xml(dictation.join("Contents/Info.plist"))?;
    plist::Value::from(
        dictionary(&[
            ("CFBundleIdentifier", "app.bex.desktop"),
            ("CFBundleName", "Bex"),
            ("CFBundleDisplayName", "Bex"),
            ("CFBundleExecutable", "Bex"),
            ("CFBundleIconFile", "Bex.icns"),
            ("CFBundlePackageType", "APPL"),
            ("CFBundleShortVersionString", "0.1.0"),
            ("CFBundleVersion", "1"),
            ("LSMinimumSystemVersion", "13.0"),
            (
                "NSMicrophoneUsageDescription",
                "音声を録音し、接続先のHostを通じて文字起こしして入力します。",
            ),
        ])
        .into_iter()
        .chain([(
            "NSHighResolutionCapable".into(),
            plist::Value::Boolean(true),
        )])
        .collect::<plist::Dictionary>(),
    )
    .to_file_xml(bundle.join("Contents/Info.plist"))?;
    sign(&dictation, &identity, None).await?;
    sign(&executables.join("Bex"), &identity, None).await?;
    sign(
        &executables.join("host-daemon"),
        &identity,
        Some("app.bex.host"),
    )
    .await?;
    sign(&bundle, &identity, None).await?;
    verify(&bundle).await?;
    let destination = target.join("Bex.app");
    let previous = staging.path().join("previous.app");
    if destination.exists() {
        fs::rename(&destination, &previous)?;
    }
    if let Err(error) = fs::rename(&bundle, &destination) {
        if previous.exists()
            && let Err(restore) = fs::rename(&previous, &destination)
        {
            // Preserve the last valid bundle if rollback itself fails.
            let saved = staging.keep();
            return Err(format!("bundle replacement failed: {error}; rollback failed: {restore}; previous bundle retained in {}", saved.display()).into());
        }
        return Err(error.into());
    }
    println!("{}", destination.display());
    Ok(())
}

fn dictionary(entries: &[(&str, &str)]) -> plist::Dictionary {
    entries
        .iter()
        .map(|(key, value)| (*key, plist::Value::String((*value).into())))
        .collect()
}
