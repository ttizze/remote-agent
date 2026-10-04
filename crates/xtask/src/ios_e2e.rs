//! Build once and own each isolated Simulator/Host pair through cleanup.
use crate::Result;
use plist::Value as Plist;
use serde_json::Value;

fn configure_run(configuration: Plist, pairing_url: &str, probe: &str) -> Result<Plist> {
    fn configure(value: Plist, url: &str, probe: &str) -> Result<(Plist, usize)> {
        match value {
            Plist::Dictionary(mut fields) if fields.contains_key("TestBundlePath") => {
                let mut environment = fields
                    .remove("EnvironmentVariables")
                    .unwrap_or_else(|| Plist::Dictionary(Default::default()));
                let variables = environment
                    .as_dictionary_mut()
                    .ok_or("invalid test environment")?;
                variables.insert("BEX_PAIRING_URL".to_owned(), Plist::String(url.to_owned()));
                variables.insert(
                    "BEX_TERMINAL_QUERY_PROBE".to_owned(),
                    Plist::String(probe.to_owned()),
                );
                fields.insert("EnvironmentVariables".to_owned(), environment);
                Ok((Plist::Dictionary(fields), 1))
            }
            Plist::Dictionary(fields) => {
                let mut configured = plist::Dictionary::new();
                let mut count = 0;
                for (name, value) in fields {
                    let (value, added) = configure(value, url, probe)?;
                    configured.insert(name, value);
                    count += added;
                }
                Ok((Plist::Dictionary(configured), count))
            }
            Plist::Array(values) => {
                let mut configured = Vec::new();
                let mut count = 0;
                for value in values {
                    let (value, added) = configure(value, url, probe)?;
                    configured.push(value);
                    count += added;
                }
                Ok((Plist::Array(configured), count))
            }
            other => Ok((other, 0)),
        }
    }
    let (configuration, count) = configure(configuration, pairing_url, probe)?;
    if count == 0 {
        return Err("xctestrun contains no test bundle".into());
    }
    Ok(configuration)
}

fn check_summary(summary: &Value, expected: usize) -> Result<()> {
    if summary.get("passedTests").and_then(Value::as_u64) != Some(expected as u64)
        || summary.get("failedTests").and_then(Value::as_u64) != Some(0)
        || summary.get("skippedTests").and_then(Value::as_u64) != Some(0)
    {
        return Err(format!(
            "iOS tests did not pass completely: expected {expected} passed, 0 failed, 0 skipped; \
             observed passed={}, failed={}, skipped={}; failures: {}",
            summary["passedTests"],
            summary["failedTests"],
            summary["skippedTests"],
            summary["testFailures"]
        )
        .into());
    }
    Ok(())
}

fn partition_tests(tests: &[String], index: usize, count: usize) -> Result<Vec<String>> {
    if index >= count || count > tests.len() {
        return Err("Test partitions must be in range and nonempty".into());
    }
    Ok(tests.iter().skip(index).step_by(count).cloned().collect())
}

fn runtime(runtimes: &Value) -> Result<String> {
    let mut selected = None;
    for entry in runtimes["runtimes"]
        .as_array()
        .ok_or("missing Simulator runtimes")?
    {
        let Some(version) = entry["version"].as_str() else {
            continue;
        };
        if entry["isAvailable"] != true || entry["platform"] != "iOS" || !version.starts_with("26.")
        {
            continue;
        }
        let version = version
            .split('.')
            .map(str::parse)
            .collect::<std::result::Result<Vec<u32>, _>>()?;
        let identifier = entry["identifier"]
            .as_str()
            .ok_or("missing Simulator runtime identifier")?;
        if selected
            .as_ref()
            .is_none_or(|(current, _)| &version > current)
        {
            selected = Some((version, identifier.to_owned()));
        }
    }
    Ok(selected
        .ok_or("Install a stable iOS 26 Simulator runtime")?
        .1)
}

use crate::{
    args,
    supervision::{self, Child, Io},
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, sync::watch, task::JoinSet};

const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
const SETUP_TIMEOUT: Duration = Duration::from_secs(300);
const SIMULATOR_DEVICE_TYPE: &str = "com.apple.CoreSimulator.SimDeviceType.iPhone-17";

#[derive(Serialize)]
struct WorkerResult {
    tests: Vec<String>,
    seconds: f64,
    #[serde(rename = "setupSeconds")]
    setup_seconds: f64,
    bundle: PathBuf,
}

#[derive(Clone)]
enum SimulatorSource {
    Template(String),
    Runtime(String),
}

fn named_devices<'a>(devices: &'a Value, name: &str) -> Result<Vec<&'a Value>> {
    let mut matching = Vec::new();
    for devices in devices["devices"]
        .as_object()
        .ok_or("missing Simulator devices")?
        .values()
    {
        for device in devices.as_array().ok_or("invalid Simulator devices")? {
            if device["name"] == name {
                matching.push(device);
            }
        }
    }
    Ok(matching)
}

async fn delete_devices(name: &str, cwd: &Path, log: &File) -> Result<()> {
    let (_sender, cancel) = watch::channel(false);
    let devices = supervision::run(
        &args!["xcrun", "simctl", "list", "devices", "-j"],
        cwd,
        Io::Capture,
        &cancel,
        Duration::from_secs(60),
    )
    .await?;
    let devices: Value = serde_json::from_slice(&devices.stdout)?;
    for device in named_devices(&devices, name)? {
        let id = device["udid"].as_str().ok_or("missing Simulator ID")?;
        let _ = supervision::run(
            &args!["xcrun", "simctl", "shutdown", id],
            cwd,
            Io::Log(log),
            &cancel,
            Duration::from_secs(60),
        )
        .await;
        supervision::run(
            &args!["xcrun", "simctl", "delete", id],
            cwd,
            Io::Log(log),
            &cancel,
            Duration::from_secs(60),
        )
        .await?;
    }
    Ok(())
}

async fn template(
    owner_root: &Path,
    runtime: &str,
    records: &Path,
    cwd: &Path,
    log: &File,
    cancel: &watch::Receiver<bool>,
) -> Result<String> {
    let owner = format!(
        "{:x}",
        Sha256::digest(owner_root.canonicalize()?.as_os_str().as_encoded_bytes())
    );
    let name = format!("Bex pristine E2E {owner} {runtime}");
    let devices = supervision::run(
        &args!["xcrun", "simctl", "list", "devices", "-j"],
        cwd,
        Io::Capture,
        cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let devices: Value = serde_json::from_slice(&devices.stdout)?;
    let matching = named_devices(&devices, &name)?;
    if !matching.is_empty() {
        if matching.len() != 1
            || matching[0]["state"] != "Shutdown"
            || matching[0]["isAvailable"] != true
        {
            return Err("Simulator template must be unique, available and shut down".into());
        }
        println!("Reusing initialized empty Simulator template");
        return Ok(matching[0]["udid"]
            .as_str()
            .ok_or("missing Simulator template ID")?
            .to_owned());
    }
    // Publish the stable name only after migration and shutdown succeed. Failed
    // preparation is deleted by its unique name, even after create cancellation.
    let preparing = format!(
        "{name} preparing {}",
        records.file_name().unwrap().to_string_lossy()
    );
    let result = async {
        let created = supervision::run(
            &args![
                "xcrun",
                "simctl",
                "create",
                &preparing,
                SIMULATOR_DEVICE_TYPE,
                runtime
            ],
            cwd,
            Io::Capture,
            cancel,
            SETUP_TIMEOUT,
        )
        .await?;
        let id = std::str::from_utf8(&created.stdout)?.trim().to_owned();
        for arguments in [
            args![vec; "xcrun", "simctl", "boot", &id],
            args![vec; "xcrun", "simctl", "bootstatus", &id, "-b"],
            args![vec; "xcrun", "simctl", "shutdown", &id],
            args![vec; "xcrun", "simctl", "rename", &id, &name],
        ] {
            supervision::run(&arguments, cwd, Io::Log(log), cancel, SETUP_TIMEOUT).await?;
        }
        println!("Initialized empty Simulator template");
        Ok(id)
    }
    .await;
    if result.is_err() {
        delete_devices(&preparing, cwd, log).await?;
    }
    result
}

async fn worker(
    tests: Vec<String>,
    target: PathBuf,
    source: PathBuf,
    simulator_source: SimulatorSource,
    prefix: PathBuf,
    without_codex: bool,
    cancel: watch::Receiver<bool>,
) -> Result<WorkerResult> {
    let started = Instant::now();
    let records = prefix.parent().ok_or("missing worker records")?;
    let label = prefix
        .file_name()
        .ok_or("missing worker label")?
        .to_string_lossy();
    let bundle = prefix.with_extension("xcresult");
    let name = format!(
        "Bex isolated E2E {}-{label}",
        records.file_name().unwrap().to_string_lossy()
    );
    let products = source.parent().ok_or("missing test products")?;
    let run = products.join(label.as_ref()).with_extension("xctestrun");
    let log = File::create(prefix.with_extension("log"))?;
    let host_log = File::create(prefix.with_extension("host.log"))?;
    let temporary = tempfile::Builder::new().prefix("bex-ios-").tempdir()?;
    let root = temporary.path();
    let cwd = std::env::current_dir()?;
    let codex = if without_codex {
        "--without-codex".into()
    } else {
        target.join("debug/bex-codex-fixture").into_os_string()
    };
    let mut command = Command::new(target.join("debug/bex-ui-fixture"));
    command
        .args(args![
            root.join("host"),
            codex,
            root.join("pairing.port"),
            "8000",
            target.join("debug/bex-claude-fixture")
        ])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(host_log.try_clone()?)
        .stderr(host_log);
    let mut host = Child::spawn(command)?;
    let result: Result<_> = async {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !root.join("pairing.port").is_file() {
                if host.try_wait()?.is_some() { return Err(format!("{label}: UI fixture exited before publishing its port").into()); }
                if Instant::now() >= deadline { return Err(format!("{label}: UI fixture timed out").into()); }
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis(100)) => {}, _ = supervision::cancelled(cancel.clone()) => return Err(supervision::interrupted()) }
            }
            let create = match simulator_source {
                SimulatorSource::Template(template) => args![vec; "xcrun", "simctl", "clone", template, &name],
                SimulatorSource::Runtime(runtime) => args![vec; "xcrun", "simctl", "create", &name, SIMULATOR_DEVICE_TYPE, runtime],
            };
            let simulator = supervision::run(&create, &cwd, Io::Capture, &cancel, SETUP_TIMEOUT).await?;
            let simulator = std::str::from_utf8(&simulator.stdout)?.trim();
            for arguments in [
                args![vec; "xcrun", "simctl", "boot", simulator],
                args![vec; "xcrun", "simctl", "bootstatus", simulator, "-b"],
                args![vec; "xcrun", "simctl", "addmedia", simulator, "apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png", records.join("attachment-video.mov")],
                args![vec; "xcrun", "simctl", "install", simulator, products.join("Debug-iphonesimulator/Bex.app")],
                args![vec; "xcrun", "simctl", "privacy", simulator, "grant", "photos-add", "com.ttizze.b-codex"],
            ] { supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, SETUP_TIMEOUT).await?; }
            let container = supervision::run(&args!["xcrun", "simctl", "get_app_container", simulator, "com.ttizze.b-codex", "data"], &cwd, Io::Capture, &cancel, SETUP_TIMEOUT).await?;
            let documents = Path::new(std::str::from_utf8(&container.stdout)?.trim()).join("Documents");
            fs::create_dir_all(&documents)?;
            fs::write(documents.join("attachment-fixture.txt"), "Isolated attachment upload fixture.\n")?;
            let pairing_url = format!("http://127.0.0.1:{}/pairing", fs::read_to_string(root.join("pairing.port"))?.trim());
            let probe = std::env::current_exe()?;
            configure_run(Plist::from_file(&source)?, &pairing_url, probe.to_str().ok_or("non-UTF-8 terminal probe path")?)?.to_file_xml(&run)?;
            let mut arguments = args![vec; "xcodebuild", "-xctestrun", &run, "-destination", format!("platform=iOS Simulator,id={simulator}"), "-parallel-testing-enabled", "NO", "-resultBundlePath", &bundle];
            arguments.extend(tests.iter().map(|test| format!("-only-testing:BexUITests/BexLaunchUITests/{test}").into()));
            arguments.push("test-without-building".into());
            let setup_seconds = started.elapsed().as_secs_f64();
            println!("{label}: Simulator and Host ready in {setup_seconds:.2}s");
            let testing = supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, BUILD_TIMEOUT);
            tokio::pin!(testing);
            let status = if std::env::var("CI").as_deref() == Ok("true") {
                tokio::select! {
                    status = &mut testing => status,
                    _ = async {
                        loop {
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            let output = fs::read_to_string(prefix.with_extension("log")).unwrap_or_default();
                            if output.contains("App event loop idle notification not received") {
                                let _ = supervision::run(
                                    &args!["/usr/bin/sample", "Bex", "2", "-file", prefix.with_extension("hang.sample.txt")],
                                    &cwd, Io::Log(&log), &cancel, Duration::from_secs(10),
                                ).await;
                                break;
                            }
                        }
                    } => testing.await,
                }
            } else { testing.await };
            if *cancel.borrow() { return Err(supervision::interrupted()); }
            let summary = supervision::run(&args!["xcrun", "xcresulttool", "get", "test-results", "summary", "--path", &bundle, "--format", "json"], &cwd, Io::Capture, &cancel, SETUP_TIMEOUT).await?;
            let summary: Value = serde_json::from_slice(&summary.stdout)?;
            fs::write(prefix.with_extension("summary.json"), format!("{}\n", serde_json::to_string_pretty(&summary)?))?;
            check_summary(&summary, tests.len())?;
            status?;
            println!("{label}: {} passed; records: {}", tests.len(), bundle.display());
            Ok(WorkerResult { tests, seconds: started.elapsed().as_secs_f64(), setup_seconds, bundle })
        }.await;
    // Stop the Host before potentially slow Simulator cleanup.
    let shutdown = host.stop(true, Duration::from_secs(10)).await;
    // Cleanup ignores cancellation, and recovers devices by this run's
    // unique name even if simctl create/clone was cancelled before returning an ID.
    let cleanup: Result<()> = async {
        match fs::remove_file(&run) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        delete_devices(&name, &cwd, &log).await
    }
    .await;
    let result = result?;
    cleanup?;
    shutdown?;
    Ok(result)
}

pub async fn run(tests: Vec<String>, without_codex: bool) -> Result<()> {
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        return Err("iOS E2E requires an Apple Silicon Mac with Xcode".into());
    }
    if tests.is_empty()
        || tests.iter().any(|test| !test.starts_with("testSimulator"))
        || tests.iter().collect::<BTreeSet<_>>().len() != tests.len()
    {
        return Err("Select unique isolated Simulator tests".into());
    }
    let shard = std::env::var("BEX_IOS_TEST_SHARD")
        .unwrap_or_else(|_| "0".to_owned())
        .parse::<usize>()?;
    let shards = std::env::var("BEX_IOS_TEST_SHARDS")
        .unwrap_or_else(|_| "1".to_owned())
        .parse::<usize>()?;
    let tests = partition_tests(&tests, shard, shards)?;
    let workers = std::env::var("BEX_IOS_TEST_WORKERS")
        .unwrap_or_else(|_| "1".to_owned())
        .parse::<usize>()?;
    if !(1..=10).contains(&workers) {
        return Err("BEX_IOS_TEST_WORKERS must be between 1 and 10".into());
    }
    let workers = workers.min(tests.len());
    let groups = (0..workers)
        .map(|index| partition_tests(&tests, index, workers))
        .collect::<Result<Vec<_>>>()?;
    let cancel = supervision::cancellation();
    let started = Instant::now();
    let cwd = std::env::current_dir()?;
    let metadata = supervision::run(
        &args!["cargo", "metadata", "--no-deps", "--format-version", "1"],
        &cwd,
        Io::Capture,
        &cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    let target = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("missing Cargo target directory")?,
    );
    let qa = target.join("qa");
    fs::create_dir_all(&qa)?;
    let records = qa.join(format!(
        "Bex-{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    ));
    fs::create_dir(&records)?;
    let build = qa.join("ios-derived-data");
    let common = supervision::run(
        &args![
            "git",
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir"
        ],
        &cwd,
        Io::Capture,
        &cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let state = PathBuf::from(std::str::from_utf8(&common.stdout)?.trim_end()).join("bex-ios-e2e");
    fs::create_dir_all(&state)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("ios-e2e.lock"))?;
    println!("Acquiring shared iOS test lock");
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis(200)) => {}, _ = supervision::cancelled(cancel.clone()) => return Err(supervision::interrupted()) }
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    let log = File::create(records.join("build.log"))?;
    let build_started = Instant::now();
    println!("Building iOS bindings and isolated Host fixtures");
    for arguments in [
        args![vec; "scripts/build-agent-ios.sh", "simulator"],
        args![vec;
            "cargo",
            "build",
            "--locked",
            "--package",
            "bex-process",
            "--bin",
            "bex-provider-supervisor",
            "--package",
            "host-fixture",
            "--bin",
            "bex-ui-fixture",
            "--bin",
            "bex-codex-fixture",
            "--bin",
            "bex-claude-fixture"
        ],
    ] {
        supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, BUILD_TIMEOUT).await?;
    }
    let products = build.join("Build/Products");
    if products.is_dir() {
        for entry in fs::read_dir(&products)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "xctestrun")
            {
                fs::remove_file(entry.path())?;
            }
        }
    }
    println!("Building the iOS app and UI test bundle");
    supervision::run(
        &args![
            "xcodebuild",
            "-skipPackagePluginValidation",
            "-project",
            "apps/mobile/iosApp/Bex.xcodeproj",
            "-scheme",
            "Bex",
            "-destination",
            "generic/platform=iOS Simulator",
            "-configuration",
            "Debug",
            "-derivedDataPath",
            &build,
            "CODE_SIGNING_ALLOWED=YES",
            "CODE_SIGN_IDENTITY=-",
            "CODE_SIGNING_REQUIRED=YES",
            format!("BEX_CARGO_TARGET_DIR={}", target.display()),
            "build-for-testing"
        ],
        &cwd,
        Io::Log(&log),
        &cancel,
        BUILD_TIMEOUT,
    )
    .await?;
    let build_seconds = build_started.elapsed().as_secs_f64();
    supervision::run(
        &args![
            "xcrun",
            "swift",
            "apps/mobile/iosApp/BexUITests/Fixtures/create-video.swift",
            records.join("attachment-video.mov")
        ],
        &cwd,
        Io::Log(&log),
        &cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let runtimes = supervision::run(
        &args!["xcrun", "simctl", "list", "runtimes", "-j"],
        &cwd,
        Io::Capture,
        &cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let runtime = runtime(&serde_json::from_slice(&runtimes.stdout)?)?;
    println!("Preparing the iOS Simulator runtime");
    let template_started = Instant::now();
    // Hosted CI devices disappear with the runner. Migrate the worker's own
    // device once instead of migrating, shutting down, cloning and booting again.
    let simulator_source = if std::env::var("CI").as_deref() == Ok("true") {
        SimulatorSource::Runtime(runtime)
    } else {
        SimulatorSource::Template(template(&state, &runtime, &records, &cwd, &log, &cancel).await?)
    };
    let template_seconds = template_started.elapsed().as_secs_f64();
    let runs = fs::read_dir(&products)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "xctestrun")
        })
        .collect::<Vec<_>>();
    if runs.len() != 1 {
        return Err("Expected exactly one xctestrun file".into());
    }
    println!(
        "Build: {build_seconds:.2}s; running {} tests with at most {workers} isolated pairs; records: {}",
        tests.len(),
        records.display()
    );
    let mut pending = JoinSet::new();
    for (index, tests) in groups.into_iter().enumerate() {
        let target = target.clone();
        let run = runs[0].clone();
        let simulator_source = simulator_source.clone();
        let records = records.clone();
        let cancel = cancel.clone();
        pending.spawn(async move {
            let mut results = Vec::new();
            // Each case owns a fresh Host database and Simulator. Import sources
            // from earlier cases must not become another case's persisted data.
            for (case, test) in tests.into_iter().enumerate() {
                results.push(
                    worker(
                        vec![test],
                        target.clone(),
                        run.clone(),
                        simulator_source.clone(),
                        records.join(format!("worker-{}-case-{}", index + 1, case + 1)),
                        without_codex,
                        cancel.clone(),
                    )
                    .await?,
                );
            }
            Result::<Vec<WorkerResult>>::Ok(results)
        });
    }
    // Await every owner so that an error never drops a live Host's cleanup.
    let mut results = Vec::new();
    let mut failure = None;
    while let Some(result) = pending.join_next().await {
        match result {
            Ok(Ok(result)) => results.extend(result),
            Ok(Err(error)) if failure.is_none() => failure = Some(error),
            Err(error) if failure.is_none() => failure = Some(error.into()),
            _ => {}
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    let seconds = started.elapsed().as_secs_f64();
    fs::write(
        records.join("summary.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(
                &json!({"buildSeconds": build_seconds, "templateSeconds": template_seconds, "seconds": seconds, "passedTests": tests.len(), "workers": results})
            )?
        ),
    )?;
    println!(
        "{} iOS UI tests passed; 0 failed, 0 skipped; {seconds:.2}s; {}",
        tests.len(),
        records.display()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    proptest! {
        #[test]
        fn partitions_cover_every_test_once_and_balance_counts(length in 1usize..64, count in 1usize..16) {
            let count = count.min(length);
            let mut tests: Vec<_> = (0..length).map(|index| format!("testSimulator{index}")).collect();
            let groups: Vec<_> = (0..count).map(|index| partition_tests(&tests, index, count).unwrap()).collect();
            let sizes: Vec<_> = groups.iter().map(Vec::len).collect();
            prop_assert!(*sizes.iter().min().unwrap() > 0);
            prop_assert!(sizes.iter().max().unwrap() - sizes.iter().min().unwrap() <= 1);
            let mut combined: Vec<_> = groups.into_iter().flatten().collect();
            combined.sort();
            tests.sort();
            prop_assert_eq!(combined, tests);
        }
    }

    #[test]
    fn invalid_partitions_fail_before_starting_workers() {
        let tests = vec!["first".to_owned(), "second".to_owned()];
        for (index, count) in [(0, 0), (1, 1), (0, 3), (usize::MAX, 2), (0, usize::MAX)] {
            assert!(partition_tests(&tests, index, count).is_err());
        }
        assert!(partition_tests(&[], 0, 1).is_err());
    }

    #[test]
    fn device_ownership_uses_the_entire_name_across_runtimes() {
        let devices = json!({"devices": {
            "current": [
                {"name": "owned", "udid": "first"},
                {"name": "owned preparing another-run", "udid": "unfinished"},
                {"name": "someone-else-owned", "udid": "external"}
            ],
            "previous": [{"name": "owned", "udid": "duplicate"}]
        }});
        let mut ids = named_devices(&devices, "owned")
            .unwrap()
            .into_iter()
            .map(|device| device["udid"].as_str().unwrap())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, ["duplicate", "first"]);
        assert!(named_devices(&devices, "unknown").unwrap().is_empty());
        assert!(named_devices(&json!({}), "owned").is_err());
        assert!(named_devices(&json!({"devices": {"current": null}}), "owned").is_err());
    }

    #[test]
    fn each_worker_preserves_configuration_and_gets_its_own_pairing() {
        let xml = br#"<?xml version="1.0"?><plist version="1.0"><dict><key>TestConfigurations</key><array><dict><key>TestTargets</key><array><dict><key>TestBundlePath</key><string>__TESTROOT__/BexUITests.xctest</string><key>EnvironmentVariables</key><dict><key>UNCHANGED</key><string>value</string></dict><key>IsUITestBundle</key><true/></dict></array></dict></array></dict></plist>"#;
        let source = Plist::from_reader(std::io::Cursor::new(xml)).unwrap();
        for index in 1..=10 {
            let url = format!("http://127.0.0.1:{}/pairing", 12300 + index);
            let mut configured = configure_run(source.clone(), &url, "/fixture/xtask").unwrap();
            let target = &mut configured.as_dictionary_mut().unwrap()["TestConfigurations"]
                .as_array_mut()
                .unwrap()[0]
                .as_dictionary_mut()
                .unwrap()["TestTargets"]
                .as_array_mut()
                .unwrap()[0];
            let environment = target.as_dictionary_mut().unwrap()["EnvironmentVariables"]
                .as_dictionary_mut()
                .unwrap();
            assert_eq!(
                environment.remove("BEX_PAIRING_URL").unwrap().as_string(),
                Some(url.as_str())
            );
            assert_eq!(
                environment
                    .remove("BEX_TERMINAL_QUERY_PROBE")
                    .unwrap()
                    .as_string(),
                Some("/fixture/xtask")
            );
            assert_eq!(configured, source);
        }
        assert!(
            configure_run(
                Plist::Dictionary(Default::default()),
                "unused",
                "/fixture/xtask"
            )
            .is_err()
        );
    }

    #[test]
    fn newest_available_ios_runtime_is_selected_numerically() {
        let entry = |version, platform, available| json!({"version": version, "platform": platform, "isAvailable": available, "identifier": version});
        assert_eq!(runtime(&json!({"runtimes": [entry("26.9", "iOS", true), entry("26.10", "iOS", true), entry("26.11", "iOS", false), entry("27.1", "iOS", true), entry("26.12", "tvOS", true)]})).unwrap(), "26.10");
        assert!(runtime(&json!({"runtimes": [entry("26.1", "iOS", false)]})).is_err());
    }

    #[test]
    fn missing_failed_and_skipped_results_cannot_pass() {
        check_summary(
            &json!({"passedTests": 2, "failedTests": 0, "skippedTests": 0}),
            2,
        )
        .unwrap();
        for summary in [
            json!({"passedTests": 1, "failedTests": 0, "skippedTests": 0}),
            json!({"passedTests": 2, "failedTests": 1, "skippedTests": 0}),
            json!({"passedTests": 2, "failedTests": 0, "skippedTests": 1}),
            json!({}),
        ] {
            assert!(check_summary(&summary, 2).is_err());
        }
        let failure = check_summary(
            &json!({"passedTests": 1, "failedTests": 1, "skippedTests": 0,
                "testFailures": [{"testName": "testSimulatorBrowser", "failureText": "startup timed out"}]}),
            2,
        )
        .unwrap_err()
        .to_string();
        assert!(failure.contains("testSimulatorBrowser"));
        assert!(failure.contains("startup timed out"));
    }
}
