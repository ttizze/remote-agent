use std::{
    fs::OpenOptions,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::Command,
};

pub(super) fn prepare_host(command: &mut Command) {
    command.process_group(0);
    let mut paths = Vec::new();
    if let Some(base) = directories::BaseDirs::new() {
        paths.push(base.home_dir().join(".local/bin"));
        paths.push(base.home_dir().join(".nix-profile/bin"));
    }
    if let Some(user) = std::env::var_os("USER") {
        paths.push(
            std::path::Path::new("/etc/profiles/per-user")
                .join(user)
                .join("bin"),
        );
    }
    paths.extend(
        [
            "/run/current-system/sw/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(Into::into),
    );
    if let Some(inherited) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&inherited));
    }
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
}

pub(super) fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options
}

pub(crate) struct Recording(std::process::ChildStdin);
impl Recording {
    pub(crate) fn finish(&mut self) -> Result<(), String> {
        use std::io::Write;
        self.0
            .write_all(b"stop\n")
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn start_recording(
    events: async_channel::Sender<super::RecordingEvent>,
) -> Result<Recording, String> {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
    };
    #[derive(serde::Deserialize)]
    struct Status {
        #[serde(default)]
        recording: bool,
        #[serde(default)]
        complete: bool,
        error: Option<String>,
    }
    let directory = tempfile::Builder::new()
        .prefix("bex-dictation-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let helper = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name("../Resources/Bex Dictation.app/Contents/MacOS/Dictation");
    let mut child = Command::new(helper)
        .arg(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let control = Recording(child.stdin.take().unwrap());
    let output = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        let result = (|| {
            for line in BufReader::new(output).lines() {
                let state: Status = serde_json::from_str(&line.map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                if let Some(error) = state.error {
                    return Err(error);
                }
                if state.recording {
                    events
                        .send_blocking(super::RecordingEvent::Started)
                        .map_err(|_| "recording cancelled")?;
                }
                if state.complete {
                    let pcm = std::fs::read(directory.path().join("recording.pcm"))
                        .map_err(|error| error.to_string())?;
                    if pcm.is_empty() || pcm.len() % 2 != 0 {
                        return Err("recording contains no valid audio".into());
                    }
                    return Ok(pcm);
                }
            }
            Err("recording interrupted".into())
        })();
        let _ = child.kill();
        let _ = child.wait();
        drop(directory);
        let _ = events.send_blocking(super::RecordingEvent::Finished(result));
    });
    Ok(control)
}
