use serde_json::Value;
use std::{
    io::Write,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Command, Stdio},
};
pub(crate) fn state_dir() -> PathBuf {
    std::env::var_os("BEX_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".bex"))
}
pub(crate) fn executable(name: &str, variable: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join(name)
        })
}
pub(crate) fn start_host(endpoint: Option<Value>) -> Result<(), String> {
    let executable = executable("host-daemon", "BEX_HOST_DAEMON");
    let state = state_dir();
    let codex = std::env::var("BEX_CODEX").unwrap_or_else(|_| "codex".into());
    let inherited = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    let search_path = format!(
        "{home}/.local/bin:{home}/.nix-profile/bin:/etc/profiles/per-user/{user}/bin:/run/current-system/sw/bin:/usr/bin:/bin:/usr/sbin:/sbin:{inherited}"
    );
    let args = [
        "--state-dir",
        state.to_str().ok_or("Invalid state directory")?,
        "--codex",
        &codex,
    ];
    if let Some(endpoint) = endpoint {
        let mut child = Command::new(&executable)
            .env("PATH", &search_path)
            .args(args)
            .arg("--configure")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(endpoint.to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
    }
    let mut child = Command::new(executable)
        .env("PATH", &search_path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(state.join("host.sock")).is_ok() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err("Host を開始できませんでした。relay の設定と Codex のインストールを確認してください。".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Err("Host の起動を確認できませんでした".into())
}
pub(crate) fn choose(mode: &str) -> Result<Option<String>, String> {
    let picker = std::env::var_os("BEX_FILE_PICKER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("../Resources/Bex File Picker.app/Contents/MacOS/FilePicker")
        });
    let output = Command::new(picker)
        .arg(mode)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("ファイル選択に失敗しました".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}
pub(crate) fn save_cache(name: &str, cache: &Value) -> Result<(), String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join(format!("{name}.tmp")))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, cache).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(dir.join(format!("{name}.tmp")), dir.join(name)).map_err(|e| e.to_string())
}
