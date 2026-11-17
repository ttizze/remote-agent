//! The Mac app as the native browser owner of an isolated test Host.

/// Opens the bex.app `scripts/build-macos.sh desktop dev` packages, scoped to
/// `state` and without starting a Host of its own. Its window registers the
/// renderer in `state` once it shows.
pub(super) fn spawn(state: &std::path::Path) -> tokio::process::Child {
    let app = agent_process::runtime_directory()
        .unwrap()
        .join("bex.app/Contents/MacOS/bex");
    tokio::process::Command::new(app)
        .env("AGENT_STATE_DIR", state)
        .env("AGENT_ISOLATED_HOST", "1")
        .env("AGENT_HOST_DAEMON", "/usr/bin/false")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}
