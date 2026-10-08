//! Resolve the FFmpeg executable shared by browser recording and native consumers.
//!
//! A release places FFmpeg beside the process executable. Development and
//! isolated tests may provide `AGENT_FFMPEG_EXECUTABLE`; when neither source
//! is available the bare platform name is left for the child process to find
//! through `PATH`.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub const EXECUTABLE_ENV: &str = "AGENT_FFMPEG_EXECUTABLE";

fn executable_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// Return the packaged sibling path for an executable, without checking the
/// filesystem. Callers that need a usable release path should check that the
/// returned file exists before selecting it.
pub fn sibling_candidate(executable: &Path) -> Option<PathBuf> {
    let directory = executable.parent()?;
    (!directory.as_os_str().is_empty()).then(|| directory.join(executable_name()))
}

/// Apply the common override, packaged sibling, then PATH lookup order.
pub fn resolve(override_path: Option<OsString>, current_executable: Option<&Path>) -> PathBuf {
    if let Some(path) = override_path.filter(|path| !path.as_os_str().is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = current_executable
        .and_then(sibling_candidate)
        .filter(|path| path.is_file())
    {
        return path;
    }
    PathBuf::from(executable_name())
}

/// Resolve FFmpeg for the current process.
pub fn executable() -> PathBuf {
    let current_executable = std::env::current_exe().ok();
    resolve(
        std::env::var_os(EXECUTABLE_ENV),
        current_executable.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::{resolve, sibling_candidate};
    use std::{ffi::OsString, fs, path::PathBuf};

    #[test]
    fn sibling_candidate_uses_the_running_executable_directory() {
        let executable = PathBuf::from(format!(
            "/opt/remote-agent/host-daemon{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(
            sibling_candidate(&executable),
            Some(PathBuf::from(format!(
                "/opt/remote-agent/ffmpeg{}",
                std::env::consts::EXE_SUFFIX
            )))
        );
    }

    #[test]
    fn resolve_prefers_override_then_existing_sibling_then_path_name() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory
            .path()
            .join(format!("host-daemon{}", std::env::consts::EXE_SUFFIX));
        let sibling = sibling_candidate(&executable).unwrap();
        fs::write(&sibling, b"fixture").unwrap();
        let override_path = directory.path().join("override-ffmpeg");
        assert_eq!(
            resolve(
                Some(OsString::from(override_path.as_os_str())),
                Some(&executable),
            ),
            override_path
        );
        assert_eq!(resolve(None, Some(&executable)), sibling);
        fs::remove_file(sibling).unwrap();
        assert_eq!(
            resolve(None, Some(&executable)),
            PathBuf::from(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX))
        );
    }
}
