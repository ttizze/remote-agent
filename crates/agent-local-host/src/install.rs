//! Replacing the installed app. The final step runs in a detached worker,
//! outside the app and Host, so neither process owns its own replacement.
use crate::{
    LocalHostRegistry, LocalHostState,
    update::{ContinuationDelivery, RECOVERY_FILE, Recovery},
};
use agent_protocol::operations::{HostUpdate, PrepareHostUpdate};
use agent_transport::transport::{Endpoint, Relays};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const RESULT_FILE: &str = "app-update-result.json";
const READY_FILE: &str = "app-update-ready.json";
/// The bundle identity a Mac update must keep.
const MAC_BUNDLE: &str = "app.bex.desktop";

/// A verified update staged beside the installed app.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub build: u64,
    pub payload: PathBuf,
    pub destination: PathBuf,
    pub backup: PathBuf,
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub isolated: bool,
}

/// The detached worker's outcome, read by the next launch.
#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateResult {
    pub build: u64,
    pub error: Option<String>,
}

fn try_update_lock(directory: &Path) -> Result<File> {
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("app-update.lock"))?;
    lock.try_lock().context("別の更新が実行中です。")?;
    Ok(lock)
}

/// The staged update for a newer build of this installation, if it is still there.
pub fn ready_plan(directory: &Path, build: u64) -> Option<Plan> {
    let plan: Plan = serde_json::from_slice(&fs::read(directory.join(READY_FILE)).ok()?).ok()?;
    (plan.build > build
        && plan.payload.exists()
        && std::env::current_exe().ok().as_ref() == Some(&plan.executable))
    .then_some(plan)
}

/// A staged update is not applied after the release it came from is deselected.
pub fn discard_ready(directory: &Path) {
    let _ = fs::remove_file(directory.join(READY_FILE));
}

/// The previous update's outcome, until `clear_result`.
pub fn last_result(directory: &Path) -> Option<UpdateResult> {
    serde_json::from_slice(&fs::read(directory.join(RESULT_FILE)).ok()?).ok()
}

pub fn clear_result(directory: &Path) {
    let _ = fs::remove_file(directory.join(RESULT_FILE));
}

/// Handle the updater's process commands before the client starts its UI or
/// connects to the Host. Returns whether this process ran such a command.
pub fn run_command(build: u64) -> bool {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next().as_deref().and_then(|value| value.to_str()) {
        Some("--bex-build-number") => {
            println!("{build}");
            true
        }
        Some("--bex-apply-update") => {
            if let Some(path) = arguments.next() {
                let runtime = tokio::runtime::Runtime::new().expect("start updater runtime");
                if let Err(error) = runtime.block_on(apply(Path::new(&path))) {
                    eprintln!(
                        "Bex update: {}",
                        agent_transport::diagnostics::sanitize(&format!("{error:#}"))
                    );
                }
            }
            true
        }
        _ => false,
    }
}

pub fn cleanup_workers(directory: &Path) {
    let Ok(_lock) = try_update_lock(directory) else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("bex-updater-")
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn run(command: &mut Command) -> Result<()> {
    ensure!(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "更新ファイルを準備できませんでした。"
    );
    Ok(())
}

fn installation_directory(executable: &Path) -> Result<&Path> {
    let parent = executable
        .parent()
        .context("application directory unavailable")?;
    if cfg!(target_os = "macos") {
        ensure!(
            parent.ends_with("Contents/MacOS"),
            "アプリケーションフォルダにインストールしたBexから更新してください。"
        );
        executable
            .ancestors()
            .nth(3)
            .context("application bundle unavailable")
    } else {
        Ok(parent)
    }
}

/// Verify the downloaded `archive` of `build` and stage it beside the
/// installed app. `directory` is the Host scope the worker restores.
pub fn stage(build: u64, archive: &Path, directory: PathBuf, isolated: bool) -> Result<Plan> {
    let executable = std::env::current_exe()?;
    let destination = installation_directory(&executable)?.to_owned();
    let parent = destination
        .parent()
        .context("application parent unavailable")?;
    // Stage beside the destination. This also checks write permission before
    // quitting and keeps the final rename on one filesystem.
    let staging = tempfile::Builder::new()
        .prefix(".bex-update-")
        .tempdir_in(parent)
        .context(
            "アプリの保存先に書き込めません。書き込み可能な場所へBexをインストールしてください。",
        )?;
    let payload = if cfg!(target_os = "macos") {
        run(Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(archive)
            .arg(staging.path()))?;
        let bundle = staging.path().join("bex.app");
        run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&bundle))?;
        run(Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute"])
            .arg(&bundle))?;
        let team = |path: &Path| -> Result<String> {
            let output = Command::new("/usr/bin/codesign")
                .args(["-d", "--verbose=4"])
                .arg(path)
                .output()?;
            ensure!(output.status.success(), "アプリの署名を確認できません。");
            let text = String::from_utf8(output.stderr)?;
            text.lines()
                .find_map(|line| line.strip_prefix("TeamIdentifier="))
                .filter(|team| *team != "not set")
                .map(str::to_owned)
                .context("署名の開発元を確認できません。")
        };
        ensure!(
            team(&bundle)? == team(&destination)?,
            "更新ファイルの開発元が一致しません。"
        );
        let output = Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleIdentifier"])
            .arg(bundle.join("Contents/Info.plist"))
            .output()?;
        ensure!(
            output.status.success() && String::from_utf8(output.stdout)?.trim() == MAC_BUNDLE,
            "更新ファイルがBexではありません。"
        );
        verify_build(&bundle.join("Contents/MacOS/bex"), build)?;
        bundle
    } else {
        let payload = staging.path().join(
            archive
                .file_name()
                .context("update file name unavailable")?,
        );
        fs::copy(archive, &payload)?;
        payload
    };
    let backup = staging.path().join("previous");
    let plan = Plan {
        build,
        payload,
        destination,
        backup,
        executable,
        directory,
        isolated,
    };
    crate::save_private_json(&plan.directory.join(READY_FILE), &plan)?;
    let _ = staging.keep();
    Ok(plan)
}

fn verify_build(executable: &Path, build: u64) -> Result<()> {
    let output = Command::new(executable)
        .arg("--bex-build-number")
        .stdin(Stdio::null())
        .output()?;
    ensure!(
        output.status.success() && String::from_utf8(output.stdout)?.trim() == build.to_string(),
        "更新後のバージョンを確認できません。更新は適用されませんでした。"
    );
    Ok(())
}

/// The caller keeps the returned lock until the process exits, after it has
/// saved and closed every window; the worker waits for its release.
pub fn launch(plan: &Plan) -> Result<File> {
    let cache = tempfile::Builder::new()
        .prefix("bex-updater-")
        .tempdir_in(&plan.directory)?;
    let path = cache.path().join("plan.json");
    crate::save_private_json(&path, plan)?;
    let lease = File::create(cache.path().join("app.lock"))?;
    lease.lock()?;
    let worker = stage_worker(&std::env::current_exe()?, cache.path())?;
    let mut command = Command::new(worker);
    // Windows keeps the current directory open. The worker must leave the
    // installed directory before replacing it, even when launched by a shortcut.
    command
        .arg("--bex-apply-update")
        .arg(&path)
        .current_dir(&plan.directory);
    detached(&mut command);
    command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cache.path().join("ready").is_file() {
        ensure!(
            Instant::now() < deadline,
            "更新用プロセスを起動できません。もう一度お試しください。"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::write(cache.path().join("apply"), b"apply")?;
    let _ = cache.keep();
    Ok(lease)
}

fn stage_worker(executable: &Path, directory: &Path) -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        // The main executable's signature seals Info.plist and the bundle's
        // resources. A bare copy cannot pass macOS validation outside its app.
        let bundle = installation_directory(executable)?;
        let worker = directory.join("updater.app");
        run(Command::new("/usr/bin/ditto").arg(bundle).arg(&worker))?;
        Ok(worker.join("Contents/MacOS").join(
            executable
                .file_name()
                .context("application executable unavailable")?,
        ))
    } else {
        let worker = directory.join(format!("updater{}", std::env::consts::EXE_SUFFIX));
        fs::copy(executable, &worker)?;
        Ok(worker)
    }
}

fn detached(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::launch::detach(command);
}

fn app_launch_command(
    destination: &Path,
    executable: &Path,
    directory: &Path,
    isolated: bool,
) -> Command {
    if cfg!(target_os = "macos") {
        // LaunchServices starts an application, rather than inheriting the
        // detached updater's process type and QoS ceiling. Keep the same state
        // scope even when the app was originally launched with custom paths.
        let mut command = Command::new("/usr/bin/open");
        command
            .args(["-n", "-a"])
            .arg(destination)
            .env("AGENT_STATE_DIR", directory)
            .env("AGENT_ISOLATED_HOST", if isolated { "1" } else { "0" });
        command
    } else {
        Command::new(executable)
    }
}

pub async fn apply(path: &Path) -> Result<()> {
    let plan: Plan = serde_json::from_slice(&fs::read(path)?)?;
    let job = path.parent().context("updater directory unavailable")?;
    let _update_lock = try_update_lock(&plan.directory)?;
    let lease = File::options()
        .read(true)
        .write(true)
        .open(job.join("app.lock"))?;
    fs::write(job.join("ready"), b"ready")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while lease.try_lock().is_err() {
        ensure!(
            Instant::now() < deadline,
            "アプリを終了できませんでした。更新は適用されませんでした。"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Consume the launcher's authorization before stopping the Host. A process
    // supervisor may start this worker again after either success or failure;
    // the same job must never interrupt conversations or relaunch the app twice.
    fs::remove_file(job.join("apply")).context("更新は開始されていないか、既に実行されました。")?;
    let registry = if plan.isolated {
        LocalHostRegistry::new(plan.directory.clone())
    } else {
        LocalHostRegistry::for_user()?
    };
    let mut result = apply_to_host(&plan, &registry, job).await;
    // Always restore the Host, including after a failed replacement. Its exact
    // launch configuration was recorded before stopping it.
    let restart = async {
        if let Ok(bytes) = fs::read(job.join("host.json")) {
            let host = registry.resolve(&plan.directory)?;
            if matches!(host.state, LocalHostState::Stopped) {
                let launch: HostUpdate = serde_json::from_slice(&bytes)?;
                let program = if verify_build(&plan.executable, plan.build).is_ok() {
                    crate::bundled_host_path(&plan.executable)
                } else {
                    PathBuf::from(launch.executable)
                };
                let mut command = Command::new(program);
                command
                    .args(launch.arguments)
                    .current_dir(launch.working_directory);
                detached(&mut command);
                command.spawn()?;
            }
            wait_for_host(&registry, &host.directory, true).await?;
            wait_for_recovery(&host.directory).await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = restart {
        result = Err(error.context("ホストの復旧を確認できませんでした。"));
    }
    let _ = fs::remove_file(plan.directory.join(READY_FILE));
    crate::save_private_json(
        &plan.directory.join(RESULT_FILE),
        &UpdateResult {
            build: plan.build,
            error: result.as_ref().err().map(|e| format!("{e:#}")),
        },
    )?;
    let mut command = app_launch_command(
        &plan.destination,
        &plan.executable,
        &plan.directory,
        plan.isolated,
    );
    detached(&mut command);
    let mut child = command
        .spawn()
        .context("更新後のBexを起動できませんでした。")?;
    if cfg!(target_os = "macos") {
        ensure!(
            child.wait()?.success(),
            "更新後のBexを起動できませんでした。"
        );
    }
    if result.is_ok() {
        if plan.backup.exists() {
            fs::remove_dir_all(&plan.backup)?;
        }
        if let Some(staging) = plan.backup.parent() {
            fs::remove_dir_all(staging)?;
        }
    }
    result
}

async fn apply_to_host(plan: &Plan, registry: &LocalHostRegistry, job: &Path) -> Result<()> {
    let host = registry.resolve(&plan.directory)?;
    match host.state {
        LocalHostState::Ready(ticket) => {
            let endpoint = Endpoint::bind(
                crate::load_local_identity(&host.directory)?,
                Relays::Loopback,
            )
            .await?;
            let connection = endpoint.connect(&ticket).await?;
            let (peer, _events) = connection.open_peer(Duration::from_secs(30), 8).await?;
            let prepared = peer.call(&PrepareHostUpdate {}).await?;
            crate::save_private_json(&job.join("host.json"), &prepared)?;
            peer.close().await;
            connection.close();
            endpoint.close().await;
            wait_for_host(registry, &host.directory, false).await?;
        }
        LocalHostState::Starting => bail!("ホストが起動中です。起動後にもう一度更新してください。"),
        LocalHostState::Stopped => {}
    }
    replace_and_verify(plan)
}

fn replace_and_verify(plan: &Plan) -> Result<()> {
    let result = (|| {
        fs::rename(&plan.destination, &plan.backup)?;
        if cfg!(target_os = "macos") {
            fs::rename(&plan.payload, &plan.destination)?;
        } else {
            run(Command::new(&plan.payload)
                .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
                .arg(format!("/DIR={}", plan.destination.display())))?;
        }
        verify_build(&plan.executable, plan.build)
    })();
    if result.is_err() && plan.backup.exists() {
        if plan.destination.exists() {
            fs::remove_dir_all(&plan.destination)?;
        }
        fs::rename(&plan.backup, &plan.destination)?;
    }
    result
}

async fn wait_for_host(registry: &LocalHostRegistry, directory: &Path, ready: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let host = registry.resolve(directory)?;
        if if ready {
            matches!(host.state, LocalHostState::Ready(_))
        } else {
            matches!(host.state, LocalHostState::Stopped)
        } {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "ホストの再起動を確認できませんでした。"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_recovery(directory: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let recovery: Recovery = serde_json::from_slice(&fs::read(directory.join(RECOVERY_FILE))?)?;
        if recovery.tasks.iter().all(|task| {
            matches!(
                task.delivery,
                ContinuationDelivery::Accepted(_) | ContinuationDelivery::Completed
            )
        }) {
            return Ok(());
        }
        ensure!(
            !recovery
                .tasks
                .iter()
                .any(|task| matches!(task.delivery, ContinuationDelivery::Failed(_))),
            "更新は適用されましたが、一部の会話を再開できませんでした。会話を開いて確認してください。"
        );
        ensure!(
            Instant::now() < deadline,
            "中断した会話の再開を確認できませんでした。会話を開いて確認してください。"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_detached_worker_preserves_its_signed_application_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("original.app");
        let executable = bundle.join("Contents/MacOS/bex");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::create_dir(bundle.join("Contents/Resources")).unwrap();
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        fs::write(
            bundle.join("Contents/Resources/sealed.txt"),
            b"signed resource",
        )
        .unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.bex.updater-test</string>
<key>CFBundleExecutable</key><string>bex</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#,
        )
        .unwrap();
        run(Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&bundle))
        .unwrap();
        let cache = directory.path().join("worker");
        fs::create_dir(&cache).unwrap();
        let worker = stage_worker(&executable, &cache).unwrap();
        run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(cache.join("updater.app")))
        .unwrap();
        run(Command::new(worker).arg("--list")).unwrap();
        assert_eq!(
            fs::read(cache.join("updater.app/Contents/Resources/sealed.txt")).unwrap(),
            b"signed resource"
        );
    }

    #[test]
    fn relaunch_preserves_the_state_scope_and_uses_the_platform_application_launcher() {
        let bundle = Path::new("/Applications/Bex Dev.app");
        let executable = bundle.join("Contents/MacOS/bex");
        let state = Path::new("/fixture/custom state");
        for isolated in [false, true] {
            let command = app_launch_command(bundle, &executable, state, isolated);
            if cfg!(target_os = "macos") {
                assert_eq!(command.get_program(), "/usr/bin/open");
                assert_eq!(
                    command.get_args().collect::<Vec<_>>(),
                    [
                        std::ffi::OsStr::new("-n"),
                        std::ffi::OsStr::new("-a"),
                        bundle.as_os_str()
                    ]
                );
                let environment: std::collections::HashMap<_, _> = command.get_envs().collect();
                assert_eq!(
                    environment[std::ffi::OsStr::new("AGENT_STATE_DIR")],
                    Some(state.as_os_str())
                );
                assert_eq!(
                    environment[std::ffi::OsStr::new("AGENT_ISOLATED_HOST")],
                    Some(std::ffi::OsStr::new(if isolated { "1" } else { "0" }))
                );
            } else {
                assert_eq!(command.get_program(), executable.as_os_str());
                assert_eq!(command.get_args().count(), 0);
            }
        }
    }

    fn plan(directory: &Path) -> Plan {
        let destination = directory.join("installed");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("previous-version"), b"old app").unwrap();
        Plan {
            build: 95,
            payload: directory.join("payload"),
            backup: directory.join("backup"),
            executable: destination.join("app"),
            destination,
            directory: directory.to_owned(),
            isolated: true,
        }
    }

    #[test]
    fn a_failed_replacement_restores_the_previous_app() {
        let directory = tempfile::tempdir().unwrap();
        let plan = plan(directory.path());
        assert!(replace_and_verify(&plan).is_err());
        assert_eq!(
            fs::read(plan.destination.join("previous-version")).unwrap(),
            b"old app"
        );
        assert!(!plan.backup.exists());
    }

    // macOS relaunches through LaunchServices, which belongs in native checks.
    // Other platforms can exercise apply's failure and replay without launching
    // a process because this fixture has no application executable.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn a_consumed_job_cannot_stop_the_host_or_overwrite_its_result() {
        let directory = tempfile::tempdir().unwrap();
        let plan = plan(directory.path());
        let path = directory.path().join("plan.json");
        crate::save_private_json(&path, &plan).unwrap();
        File::create(directory.path().join("app.lock")).unwrap();
        fs::write(directory.path().join("apply"), b"apply").unwrap();

        assert!(apply(&path).await.is_err());
        assert!(last_result(directory.path()).unwrap().error.is_some());
        assert!(!directory.path().join("apply").exists());
        let result = fs::read(directory.path().join(RESULT_FILE)).unwrap();

        // Restoring the payload cannot authorize the consumed job again.
        fs::create_dir(&plan.payload).unwrap();
        let _host = LocalHostRegistry::new(plan.directory.clone())
            .acquire(&plan.directory)
            .unwrap();

        assert!(
            apply(&path)
                .await
                .unwrap_err()
                .to_string()
                .contains("既に実行")
        );
        assert_eq!(
            fs::read(directory.path().join(RESULT_FILE)).unwrap(),
            result
        );
        assert!(!directory.path().join("host.json").exists());
        assert!(plan.destination.join("previous-version").is_file());
        assert!(plan.payload.is_dir());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_wrong_installed_version_rolls_back_and_a_verified_version_keeps_its_backup() {
        use std::os::unix::fs::PermissionsExt;
        for (version, success) in [(94, false), (95, true)] {
            let directory = tempfile::tempdir().unwrap();
            let plan = plan(directory.path());
            fs::create_dir(&plan.payload).unwrap();
            let app = plan.payload.join("app");
            fs::write(&app, format!("#!/bin/sh\nprintf '%s\\n' {version}\n")).unwrap();
            fs::set_permissions(app, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(replace_and_verify(&plan).is_ok(), success);
            assert_eq!(plan.backup.exists(), success);
            assert_eq!(plan.destination.join("previous-version").exists(), !success);
        }
    }
}
