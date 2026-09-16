use anyhow::{Context, Result};
use std::{fs, io, path::Path};

/// Unix state is owner-only; Windows directories inherit the parent DACL.
/// The default state parent is the current user's local application data folder.
pub fn create_state_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn worktree_directory(parent: &Path) -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("session-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

pub(crate) fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Resolve the interactive shell on the Host, which may run a different OS
/// from the client opening the terminal.
pub(crate) fn terminal_command() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["cmd.exe"]
    }
    #[cfg(not(windows))]
    {
        &["/bin/sh", "-c", "exec \"${SHELL:-/bin/sh}\" -l"]
    }
}

/// Atomically replace owner-only JSON state and flush its contents before rename.
pub fn save_private_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    create_state_directory(parent)?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write_with_options(
            |file| {
                serde_json::to_writer(&mut *file, value)?;
                file.sync_all()
            },
            private_file_options(),
        )
        .map_err(Into::into)
}
