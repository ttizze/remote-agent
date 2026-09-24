use agent_peer::{expand_path, home};
use anyhow::{Context, Result, ensure};
use clap::{Command, arg};
use std::{env, fs, os::unix::fs::symlink, path::Path};

fn main() -> Result<()> {
    let args = Command::new("agent-peer-install-skills")
        .about("Link the packaged skill into the selected Codex and Claude homes")
        .args([
            arg!(--"codex-home" <PATH>).env("CODEX_HOME"),
            arg!(--"claude-home" <PATH>).env("CLAUDE_CONFIG_DIR"),
        ])
        .get_matches();
    let executable = env::current_exe()?.canonicalize()?;
    let prefix = executable
        .parent()
        .and_then(|p| p.parent())
        .context("No installation prefix")?;
    let source = prefix.join("share/agent-peer");
    ensure!(
        source.join("SKILL.md").is_file(),
        "The package has no SKILL.md"
    );
    let mut destinations = Vec::new();
    for (option, default) in [("codex-home", ".codex"), ("claude-home", ".claude")] {
        let path = match args.get_one::<String>(option).filter(|p| !p.is_empty()) {
            Some(path) => expand_path(Path::new(path))?,
            None => home()?.join(default),
        };
        let destination = std::path::absolute(path)?.join("skills/agent-peer");
        match destination.symlink_metadata() {
            Ok(meta) => ensure!(
                meta.file_type().is_symlink(),
                "Refusing to replace non-symlink: {}",
                destination.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        destinations.push(destination);
    }
    for destination in destinations {
        let parent = destination.parent().expect("skill has parent");
        fs::create_dir_all(parent)?;
        let staging = tempfile::Builder::new()
            .prefix(".agent-peer-")
            .tempdir_in(parent)?;
        let link = staging.path().join("skill");
        symlink(&source, &link)?;
        fs::rename(link, &destination)?;
        println!("{} -> {}", destination.display(), source.display());
    }
    Ok(())
}
