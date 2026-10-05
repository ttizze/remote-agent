//! Build once and own each isolated Simulator/Host pair through cleanup.
use crate::Result;
use plist::Value as Plist;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestDriver {
    XCTest,
    Maestro,
}

fn check_maestro_report(report: &str, tests: &[String]) -> Result<()> {
    let document = roxmltree::Document::parse(report)?;
    let cases = document
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
        .collect::<Vec<_>>();
    let names = cases
        .iter()
        .filter_map(|node| node.attribute("name"))
        .collect::<std::collections::BTreeSet<_>>();
    if !document.root_element().has_tag_name("testsuites")
        || cases.len() != tests.len()
        || names != tests.iter().map(String::as_str).collect()
        || cases
            .iter()
            .any(|node| node.attribute("status") != Some("SUCCESS"))
        || document.descendants().any(|node| {
            node.has_tag_name("failure")
                || node.has_tag_name("error")
                || node.has_tag_name("skipped")
                || (node.has_tag_name("testsuite") && node.attribute("failures") != Some("0"))
        })
    {
        return Err(
            "Maestro must complete every selected flow successfully, without skipped flows".into(),
        );
    }
    Ok(())
}

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

/// Count failures by test across every worker summary, so repeated rounds expose flaky cases.
fn failure_counts(summaries: &[Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for summary in summaries {
        // One run may report several failures for the same test.
        let failed = summary["testFailures"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|failure| failure["testName"].as_str())
            .collect::<BTreeSet<_>>();
        for name in failed {
            *counts.entry(name.to_owned()).or_default() += 1;
        }
    }
    counts
}

fn partition_tests(tests: &[String], index: usize, count: usize) -> Result<Vec<String>> {
    if index >= count || count > tests.len() {
        return Err("Test partitions must be in range and nonempty".into());
    }
    Ok(tests.iter().skip(index).step_by(count).cloned().collect())
}

fn needs_media_fixtures(tests: &[String]) -> bool {
    tests.iter().any(|test| {
        matches!(
            test.as_str(),
            "testSimulatorCanAddASecondPhoto"
                | "testSimulatorCanAttachPhotosAndVideos"
                | "testSimulatorRetriesPhotoUploadAfterWorkspaceRecovery"
        )
    })
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
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, sync::watch, task::JoinSet};

const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
const SETUP_TIMEOUT: Duration = Duration::from_secs(300);
const SIMULATOR_DEVICE_TYPE: &str = "com.apple.CoreSimulator.SimDeviceType.iPhone-17";

fn simulator_app_pid(processes: &str, simulator: &str) -> Option<u32> {
    let device = format!("/Devices/{simulator}/");
    processes.lines().find_map(|line| {
        if line.contains(&device) && line.trim_end().ends_with("/Bex.app/Bex") {
            line.split_whitespace().next()?.parse().ok()
        } else {
            None
        }
    })
}

/// Sample a stalled test before XCTest terminates the app. Never inspect another worker.
async fn monitor_diagnostics(
    simulator: &str,
    prefix: &Path,
    cwd: &Path,
    cancel: &watch::Receiver<bool>,
) -> Result<()> {
    let mut recorded_startup = false;
    let mut sampled = BTreeSet::new();
    let timeout = Duration::from_secs(30);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let progress = fs::metadata(prefix.with_extension("log"))?.modified()?;
        let quiet = progress.elapsed().unwrap_or_default();
        if recorded_startup && quiet < Duration::from_secs(30) {
            continue;
        }
        let processes = match supervision::run(
            &args!["/bin/ps", "-axo", "pid,ppid,rss,pcpu,comm", "-m"],
            cwd,
            Io::Capture,
            cancel,
            timeout,
        )
        .await
        {
            Ok(processes) => processes,
            Err(error) => {
                eprintln!("Simulator {simulator}: diagnostic process listing failed: {error}");
                continue;
            }
        };
        let Some(pid) = simulator_app_pid(std::str::from_utf8(&processes.stdout)?, simulator)
        else {
            continue;
        };
        if !recorded_startup {
            recorded_startup = true;
            fs::write(
                prefix.with_extension("startup-processes.txt"),
                &processes.stdout,
            )?;
            let memory = File::create(prefix.with_extension("startup-memory.txt"))?;
            if let Err(error) = supervision::run(
                &args!["/usr/bin/vm_stat"],
                cwd,
                Io::Log(&memory),
                cancel,
                timeout,
            )
            .await
            {
                eprintln!("Simulator {simulator}: diagnostic memory counters failed: {error}");
            }
        }
        if quiet < Duration::from_secs(30)
            || fs::metadata(prefix.with_extension("log"))?.modified()? != progress
            || !sampled.insert(pid)
        {
            continue;
        }
        fs::write(
            prefix.with_extension(format!("hang-{pid}-processes.txt")),
            &processes.stdout,
        )?;
        let mut output =
            File::create(prefix.with_extension(format!("hang-{pid}-sample-output.txt")))?;
        // Hosted macOS permits passwordless diagnostics of this worker's Debug app.
        if let Err(error) = supervision::run(
            &args![
                "sudo",
                "-n",
                "/usr/bin/sample",
                pid.to_string(),
                "3",
                "-file",
                prefix.with_extension(format!("hang-{pid}-sample.txt"))
            ],
            cwd,
            Io::Log(&output),
            cancel,
            timeout,
        )
        .await
        {
            writeln!(output, "Diagnostic sampling failed: {error}")?;
        }
    }
}

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

#[expect(
    clippy::too_many_arguments,
    reason = "Keep each isolated worker's resource and driver inputs explicit"
)]
async fn worker(
    tests: Vec<String>,
    target: PathBuf,
    source: PathBuf,
    simulator_source: SimulatorSource,
    prefix: PathBuf,
    without_codex: bool,
    driver: TestDriver,
    cancel: watch::Receiver<bool>,
) -> Result<WorkerResult> {
    let started = Instant::now();
    let records = prefix.parent().ok_or("missing worker records")?;
    let label = prefix
        .file_name()
        .ok_or("missing worker label")?
        .to_string_lossy();
    let bundle = prefix.with_extension(if driver == TestDriver::Maestro {
        "maestro"
    } else {
        "xcresult"
    });
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
    if std::env::var("CI").as_deref() == Ok("true")
        && tests
            .iter()
            .any(|test| test == "testSimulatorBrowserIsSeparateFromConversationAndPreservesPage")
    {
        command.arg("--prepare-browser");
    }
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
            let mut preparation = vec![
                ("boot", args![vec; "xcrun", "simctl", "boot", simulator]),
                ("bootstatus", args![vec; "xcrun", "simctl", "bootstatus", simulator, "-b"]),
            ];
            if needs_media_fixtures(&tests) {
                preparation.push(("addmedia", args![vec; "xcrun", "simctl", "addmedia", simulator, "apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png", records.join("attachment-video.mov")]));
            }
            preparation.push(("install", args![vec; "xcrun", "simctl", "install", simulator, products.join("Debug-iphonesimulator/Bex.app")]));
            if tests.iter().any(|test| test == "testSimulatorOpensOnlyTheTappedImageAndSavesIt") {
                preparation.push(("photos-add permission", args![vec; "xcrun", "simctl", "privacy", simulator, "grant", "photos-add", "com.ttizze.b-codex"]));
            }
            for (phase, arguments) in preparation {
                println!("{label}: {phase}");
                supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, SETUP_TIMEOUT).await
                    .map_err(|error| format!("{label}: {phase} failed: {error}"))?;
            }
            if tests.iter().any(|test| test == "testSimulatorCanAttachDownloadAndPrepareAIEdit") {
                let container = supervision::run(&args!["xcrun", "simctl", "get_app_container", simulator, "com.ttizze.b-codex", "data"], &cwd, Io::Capture, &cancel, SETUP_TIMEOUT).await?;
                let documents = Path::new(std::str::from_utf8(&container.stdout)?.trim()).join("Documents");
                fs::create_dir_all(&documents)?;
                fs::write(documents.join("attachment-fixture.txt"), "Isolated attachment upload fixture.\n")?;
            }
            let pairing_url = format!("http://127.0.0.1:{}/pairing", fs::read_to_string(root.join("pairing.port"))?.trim());
            if driver == TestDriver::Maestro {
                fs::create_dir(&bundle)?;
                let report = bundle.join("report.xml");
                // Match Maestro's ephemeral-port selection, then explicitly
                // share that endpoint with the flow's loopback API requests.
                let driver_port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?
                    .local_addr()?
                    .port();
                let mut arguments = args![vec; "nix", "run", ".#maestro", "--", "--device", simulator, "test", "--driver-host-port", driver_port.to_string(), "--config", "apps/mobile/maestro/ios/config.yaml", "--format", "junit", "--output", &report, "--test-output-dir", &bundle, "--env", format!("BEX_PAIRING_URL={pairing_url}"), "--env", format!("BEX_IOS_DRIVER_URL=http://127.0.0.1:{driver_port}")];
                arguments.extend(tests.iter().map(|test| cwd.join("apps/mobile/maestro/ios").join(test).with_extension("yaml").into_os_string()));
                let setup_seconds = started.elapsed().as_secs_f64();
                println!("{label}: Simulator and Host ready in {setup_seconds:.2}s");
                supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, BUILD_TIMEOUT).await?;
                check_maestro_report(&fs::read_to_string(&report)?, &tests)?;
                println!("{label}: {} Maestro flows passed; records: {}", tests.len(), bundle.display());
                return Ok(WorkerResult { tests, seconds: started.elapsed().as_secs_f64(), setup_seconds, bundle });
            }
            let probe = std::env::current_exe()?;
            configure_run(Plist::from_file(&source)?, &pairing_url, probe.to_str().ok_or("non-UTF-8 terminal probe path")?)?.to_file_xml(&run)?;
            let mut arguments = args![vec; "xcodebuild", "-xctestrun", &run, "-destination", format!("platform=iOS Simulator,id={simulator}"), "-parallel-testing-enabled", "NO", "-collect-test-diagnostics", "on-failure", "-resultBundlePath", &bundle];
            arguments.extend(tests.iter().map(|test| format!("-only-testing:BexUITests/BexLaunchUITests/{test}").into()));
            arguments.push("test-without-building".into());
            let setup_seconds = started.elapsed().as_secs_f64();
            println!("{label}: Simulator and Host ready in {setup_seconds:.2}s");
            let acceptance = supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, BUILD_TIMEOUT);
            tokio::pin!(acceptance);
            let status = if std::env::var("CI").as_deref() == Ok("true") {
                tokio::select! {
                    status = &mut acceptance => status,
                    diagnostics = monitor_diagnostics(simulator, &prefix, &cwd, &cancel) => {
                        if let Err(error) = diagnostics {
                            eprintln!("{label}: diagnostic monitor stopped: {error}");
                        }
                        acceptance.await
                    }
                }
            } else {
                acceptance.await
            };
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
        delete_devices(&name, &cwd, &log).await?;
        let diagnostics = root.join("host/logs/host.jsonl");
        if diagnostics.is_file() {
            fs::copy(diagnostics, prefix.with_extension("host-diagnostics.jsonl"))?;
        }
        Ok(())
    }
    .await;
    let result = result?;
    cleanup?;
    shutdown?;
    Ok(result)
}

pub async fn run(tests: Vec<String>, without_codex: bool, driver: TestDriver) -> Result<()> {
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
    // Repeated rounds measure flaky tests; each round gets fresh Simulators and Hosts.
    let rounds = std::env::var("BEX_IOS_TEST_ROUNDS")
        .unwrap_or_else(|_| "1".to_owned())
        .parse::<usize>()?;
    if !(1..=10).contains(&rounds) {
        return Err("BEX_IOS_TEST_ROUNDS must be between 1 and 10".into());
    }
    if driver == TestDriver::Maestro {
        for test in &tests {
            if !Path::new("apps/mobile/maestro/ios")
                .join(test)
                .with_extension("yaml")
                .is_file()
            {
                return Err(format!("No maintained Maestro flow for {test}").into());
            }
        }
    }
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
    if driver == TestDriver::Maestro {
        supervision::run(
            &args!["nix", "run", ".#maestro", "--", "--version"],
            &cwd,
            Io::Log(&log),
            &cancel,
            BUILD_TIMEOUT,
        )
        .await?;
    }
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
            "ARCHS=arm64",
            "ONLY_ACTIVE_ARCH=YES",
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
    if needs_media_fixtures(&tests) {
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
    }
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
        "Build: {build_seconds:.2}s; running {} tests on {workers} isolated pairs; records: {}",
        tests.len(),
        records.display()
    );
    let mut results = Vec::new();
    let mut failure = None;
    for round in 1..=rounds {
        let mut pending = JoinSet::new();
        for (index, tests) in groups.iter().cloned().enumerate() {
            let label = if rounds == 1 {
                format!("worker-{}", index + 1)
            } else {
                format!("worker-{}-round-{round}", index + 1)
            };
            pending.spawn(worker(
                tests,
                target.clone(),
                runs[0].clone(),
                simulator_source.clone(),
                records.join(label),
                without_codex,
                driver,
                cancel.clone(),
            ));
        }
        // Await every owner so that an error never drops a live Host's cleanup.
        while let Some(result) = pending.join_next().await {
            match result {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(error)) => {
                    if rounds > 1 {
                        eprintln!("Round {round}/{rounds}: {error}");
                    }
                    failure.get_or_insert(error);
                }
                Err(error) => {
                    failure.get_or_insert(error.into());
                }
            }
        }
        if *cancel.borrow() {
            return Err(supervision::interrupted());
        }
    }
    if rounds > 1 {
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&records)? {
            let path = entry?.path();
            if path.to_string_lossy().ends_with(".summary.json") {
                summaries.push(serde_json::from_slice(&fs::read(path)?)?);
            }
        }
        let counts = failure_counts(&summaries);
        println!(
            "{rounds} rounds of {} tests: {} worker results, {} failing tests",
            tests.len(),
            summaries.len(),
            counts.len()
        );
        for (test, failed) in &counts {
            println!("  {test}: failed {failed}/{rounds}");
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
        fn maestro_requires_the_selected_successful_flows(
            count in 2usize..16,
            status in prop::sample::select(vec!["PENDING", "PREPARING", "INSTALLING", "RUNNING", "ERROR", "CANCELED", "STOPPED", "WARNING"]),
        ) {
            let tests: Vec<_> = (0..count).map(|index| format!("testSimulator{index}")).collect();
            let cases = tests.iter().rev().map(|name| format!("<testcase name=\"{name}\" status=\"SUCCESS\"/>")).collect::<String>();
            let report = format!("<testsuites><testsuite tests=\"{count}\" failures=\"0\">{cases}</testsuite></testsuites>");
            prop_assert!(check_maestro_report(&report, &tests).is_ok());
            let incomplete = report.replacen("status=\"SUCCESS\"", &format!("status=\"{status}\""), 1);
            prop_assert!(check_maestro_report(&incomplete, &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("name=\"testSimulator0\"", "name=\"other\""), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("name=\"testSimulator0\"", "name=\"testSimulator1\""), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("<testcase name=\"testSimulator0\" status=\"SUCCESS\"/>", ""), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("</testsuite>", "<failure/></testsuite>"), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("</testsuite>", "<error/></testsuite>"), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("</testsuite>", "<skipped/></testsuite>"), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("failures=\"0\"", "failures=\"1\""), &tests).is_err());
            prop_assert!(check_maestro_report(&report.replace("testsuites", "other"), &tests).is_err());
            prop_assert!(check_maestro_report("not xml", &tests).is_err());
        }

        #[test]
        fn diagnostic_sampling_stays_with_its_simulator(pid in 1u32..100_000, simulator in "[A-F0-9-]{36}") {
            let processes = format!(
                "44 1 100 99 /Devices/{simulator}0/data/Applications/Bex.app/Bex\n\
                 45 1 100 99 /Devices/{simulator}/data/Applications/Bex.app/BexUITests-Runner\n\
                 46 1 100 99 /Devices/{simulator}/data/Applications/Bex.app/BexHelper\n\
                 {pid} 1 100 99 /Devices/{simulator}/data/Applications/Bex.app/Bex\n"
            );
            prop_assert_eq!(simulator_app_pid(&processes, &simulator), Some(pid));
            prop_assert_eq!(simulator_app_pid(&processes, "other-device"), None);
            prop_assert_eq!(simulator_app_pid("", &simulator), None);
        }

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
    fn only_photo_picker_tests_need_seeded_media() {
        let conversation = vec![
            "testSimulatorOpensLongInterruptedHistoryAtLatestMessage".to_owned(),
            "testSimulatorOpensOnlyTheTappedImageAndSavesIt".to_owned(),
            "testSimulatorCanAttachDownloadAndPrepareAIEdit".to_owned(),
        ];
        assert!(!needs_media_fixtures(&conversation));
        for test in [
            "testSimulatorCanAddASecondPhoto",
            "testSimulatorCanAttachPhotosAndVideos",
            "testSimulatorRetriesPhotoUploadAfterWorkspaceRecovery",
        ] {
            let mut mixed = conversation.clone();
            mixed.push(test.to_owned());
            assert!(needs_media_fixtures(&mixed));
        }
    }

    #[test]
    fn failure_counts_add_each_failing_run_once_per_test() {
        let failure = |name: &str| json!({"testName": name, "failureText": "timed out"});
        let summaries = [
            json!({"testFailures": [failure("first"), failure("first"), failure("second")]}),
            json!({"testFailures": [failure("first")]}),
            json!({"testFailures": []}),
            json!({"passedTests": 3}),
        ];
        assert_eq!(
            failure_counts(&summaries).into_iter().collect::<Vec<_>>(),
            [("first".to_owned(), 2), ("second".to_owned(), 1)]
        );
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
