//! All provider processes run under a small companion supervisor. Its stdin is
//! a lifetime pipe owned by the Host; EOF also covers Host SIGKILL/crashes.
use std::{io, path::Path};
use tokio::process::Command;

pub fn command(program: &Path) -> io::Result<Command> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("Host executable directory is unavailable"))?;
    let directory = if directory
        .file_name()
        .is_some_and(|name| name == "deps" || name == "examples")
    {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    };
    let supervisor = directory.join(format!(
        "bex-provider-supervisor{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !supervisor.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "bex-provider-supervisor must be built and installed beside the Host; provider execution is unavailable",
        ));
    }
    let mut command = Command::new(supervisor);
    command.arg(program);
    // Do not kill the supervisor on drop: closing its input lets it terminate
    // and reap the entire provider process group first.
    command.kill_on_drop(false);
    Ok(command)
}
