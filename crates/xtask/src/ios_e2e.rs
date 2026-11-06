//! Build once and own each isolated Simulator/Host pair through cleanup.
use crate::Result;
use plist::Value as Plist;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestDriver {
    XCTest,
    Maestro,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    BuildAndTest,
    BuildOnly,
    Prebuilt,
    AwaitBuild,
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
                variables.insert(
                    "AGENT_PAIRING_URL".to_owned(),
                    Plist::String(url.to_owned()),
                );
                variables.insert(
                    "AGENT_TERMINAL_QUERY_PROBE".to_owned(),
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
                | "testSimulatorRetriesPhotoUploadAfterStorageRecovery"
        )
    })
}

fn needs_browser_fixture(tests: &[String]) -> bool {
    tests
        .iter()
        .any(|test| test == "testSimulatorBrowserUsesOneSharedImportMenu")
}

fn needs_file_preview_fixtures(tests: &[String]) -> bool {
    tests
        .iter()
        .any(|test| test == "testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening")
}

/// Select the newest stable iOS 26 runtime: its identifier and root file system.
fn runtime(runtimes: &Value) -> Result<(String, PathBuf)> {
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
        let root = entry["runtimeRoot"]
            .as_str()
            .ok_or("missing Simulator runtime root")?;
        if selected
            .as_ref()
            .is_none_or(|(current, _, _)| &version > current)
        {
            selected = Some((version, identifier.to_owned(), PathBuf::from(root)));
        }
    }
    let (_, identifier, root) = selected.ok_or("Install a stable iOS 26 Simulator runtime")?;
    Ok((identifier, root))
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
/// A fresh Simulator starts about 270 processes; on a 7 GiB CI Mac their
/// first-boot work compresses memory until Host replies take 10-20 seconds.
/// Bex uses only AVFoundation, Photos and QuickLook, so stop the system
/// services for widgets, mail, maps, watch, health, Siri, Spotlight and stores.
/// Paths are relative to the runtime root.
const IDLE_SIMULATOR_SERVICES: &[&str] = &[
    "System/Library/LaunchDaemons/com.apple.GameController.gamecontrollerd.plist",
    "System/Library/LaunchDaemons/com.apple.Maps.mapssyncd.plist",
    "System/Library/LaunchDaemons/com.apple.SafariBookmarksSyncAgent.plist",
    "System/Library/LaunchDaemons/com.apple.ScreenTimeAgent.plist",
    "System/Library/LaunchDaemons/com.apple.WebBookmarks.webbookmarksd.plist",
    "System/Library/LaunchDaemons/com.apple.activityawardsd.plist",
    "System/Library/LaunchDaemons/com.apple.activitysharingd.plist",
    "System/Library/LaunchDaemons/com.apple.amsengagementd.plist",
    "System/Library/LaunchDaemons/com.apple.ap.adprivacyd.plist",
    "System/Library/LaunchDaemons/com.apple.ap.promotedcontentd.plist",
    "System/Library/LaunchDaemons/com.apple.appstored.plist",
    "System/Library/LaunchDaemons/com.apple.askpermissiond.plist",
    "System/Library/LaunchDaemons/com.apple.assistant_cdmd.plist",
    "System/Library/LaunchDaemons/com.apple.assistant_service.plist",
    "System/Library/LaunchDaemons/com.apple.assistantd.plist",
    "System/Library/LaunchDaemons/com.apple.avatarsd.plist",
    "System/Library/LaunchDaemons/com.apple.chronod.plist",
    "System/Library/LaunchDaemons/com.apple.contacts.postersyncd.plist",
    "System/Library/LaunchDaemons/com.apple.dataaccess.dataaccessd.plist",
    "System/Library/LaunchDaemons/com.apple.email.maild.plist",
    "System/Library/LaunchDaemons/com.apple.familycircled.plist",
    "System/Library/LaunchDaemons/com.apple.financed.plist",
    "System/Library/LaunchDaemons/com.apple.findmy.findmylocated.plist",
    "System/Library/LaunchDaemons/com.apple.fitcore.plist",
    "System/Library/LaunchDaemons/com.apple.fitnesscoachingd.plist",
    "System/Library/LaunchDaemons/com.apple.fitnessintelligenced.plist",
    "System/Library/LaunchDaemons/com.apple.generativeexperiencesd.plist",
    "System/Library/LaunchDaemons/com.apple.geoanalyticsd.plist",
    "System/Library/LaunchDaemons/com.apple.healthappd.plist",
    "System/Library/LaunchDaemons/com.apple.healthd.plist",
    "System/Library/LaunchDaemons/com.apple.homed.plist",
    "System/Library/LaunchDaemons/com.apple.icloudmailagent.plist",
    "System/Library/LaunchDaemons/com.apple.intelligenceplatformd.plist",
    "System/Library/LaunchDaemons/com.apple.itunescloudd.plist",
    "System/Library/LaunchDaemons/com.apple.itunesstored.plist",
    "System/Library/LaunchDaemons/com.apple.mediaanalysisd.plist",
    "System/Library/LaunchDaemons/com.apple.mobiletimerd.plist",
    "System/Library/LaunchDaemons/com.apple.modelcatalogd.plist",
    "System/Library/LaunchDaemons/com.apple.modelmanagerd.plist",
    "System/Library/LaunchDaemons/com.apple.nanotimekitcompaniond.plist",
    "System/Library/LaunchDaemons/com.apple.navd.plist",
    "System/Library/LaunchDaemons/com.apple.newsd.plist",
    "System/Library/LaunchDaemons/com.apple.parsec-fbf.plist",
    "System/Library/LaunchDaemons/com.apple.parsecd.plist",
    "System/Library/LaunchDaemons/com.apple.passd.plist",
    "System/Library/LaunchDaemons/com.apple.photoanalysisd.plist",
    "System/Library/LaunchDaemons/com.apple.remindd.plist",
    "System/Library/LaunchDaemons/com.apple.routined.plist",
    "System/Library/LaunchDaemons/com.apple.searchd.plist",
    "System/Library/LaunchDaemons/com.apple.siriactionsd.plist",
    "System/Library/LaunchDaemons/com.apple.siriinferenced.plist",
    "System/Library/LaunchDaemons/com.apple.siriknowledged.plist",
    "System/Library/LaunchDaemons/com.apple.sirittsd.plist",
    "System/Library/LaunchDaemons/com.apple.sleepd.plist",
    "System/Library/LaunchDaemons/com.apple.spotlightknowledged.updater.plist",
    "System/Library/LaunchDaemons/com.apple.suggestd.plist",
    "System/Library/LaunchDaemons/com.apple.textunderstandingd.plist",
    "System/Library/LaunchDaemons/com.apple.translationd.plist",
    "System/Library/LaunchDaemons/com.apple.triald.plist",
    "System/Library/LaunchDaemons/com.apple.tvremoted.plist",
    "System/Library/LaunchDaemons/com.apple.voicebankingd.plist",
    "System/Library/LaunchDaemons/com.apple.wcd.plist",
    "System/Library/LaunchDaemons/com.apple.weatherd.plist",
    "System/Library/NanoLaunchDaemons/com.apple.addressbooksyncd.plist",
    "System/Library/NanoLaunchDaemons/com.apple.appconduitd.plist",
    "System/Library/NanoLaunchDaemons/com.apple.brook.brookcompaniond.plist",
    "System/Library/NanoLaunchDaemons/com.apple.companionappd.plist",
    "System/Library/NanoLaunchDaemons/com.apple.eventkitsyncd.plist",
    "System/Library/NanoLaunchDaemons/com.apple.nanoappregistryd.plist",
    "System/Library/NanoLaunchDaemons/com.apple.nanomapscd.plist",
];
/// System apps that the first boot opens in the background.
const IDLE_SIMULATOR_APPS: &[&str] = &[
    "com.apple.PosterBoard",
    "com.apple.Spotlight",
    "com.apple.mobilecal",
    "com.apple.family",
];

fn service_label(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".plist"))
        .unwrap_or(path)
}

/// Stop unused system services and return how many of the runtime's services stopped.
async fn quiet_simulator(
    simulator: &str,
    runtime_root: &Path,
    cwd: &Path,
    log: &File,
    cancel: &watch::Receiver<bool>,
) -> Result<(usize, usize)> {
    let timeout = Duration::from_secs(60);
    let services = IDLE_SIMULATOR_SERVICES
        .iter()
        .filter(|path| runtime_root.join(path).is_file())
        .collect::<Vec<_>>();
    // Independent app terminations need not wait for every service to exit.
    // Keep at most ten CoreSimulator requests active on a loaded CI Mac.
    let commands = services
        .chunks(services.len().div_ceil(10).max(1))
        .map(|chunk| {
            let mut arguments =
                args![vec; "xcrun", "simctl", "spawn", simulator, "launchctl", "bootout", "system"];
            arguments.extend(
                chunk
                    .iter()
                    .map(|path| runtime_root.join(path).into_os_string()),
            );
            (arguments, SETUP_TIMEOUT)
        })
        .chain(IDLE_SIMULATOR_APPS.iter().map(|app| {
            (
                args![vec; "xcrun", "simctl", "terminate", simulator, *app],
                timeout,
            )
        }));
    let mut pending = JoinSet::new();
    for (arguments, limit) in commands {
        let (cwd, log, cancel) = (cwd.to_owned(), log.try_clone()?, cancel.clone());
        // Services/apps that never started may fail; inspect the final listing below.
        pending.spawn(async move {
            let _ = supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, limit).await;
        });
        if pending.len() == 10 {
            pending.join_next().await;
        }
    }
    while pending.join_next().await.is_some() {}
    let listed = supervision::run(
        &args!["xcrun", "simctl", "spawn", simulator, "launchctl", "list"],
        cwd,
        Io::Capture,
        cancel,
        timeout,
    )
    .await?;
    let listed = std::str::from_utf8(&listed.stdout)?
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .collect::<BTreeSet<_>>();
    let running = services
        .iter()
        .filter(|path| listed.contains(service_label(path)))
        .count();
    Ok((services.len() - running, services.len()))
}

fn simulator_app_pid(processes: &str, simulator: &str) -> Option<u32> {
    let device = format!("/Devices/{simulator}/");
    processes.lines().find_map(|line| {
        if line.contains(&device) && line.trim_end().ends_with("/bex.app/bex") {
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
    let owner = crate::hex(&Sha256::digest(
        owner_root.canonicalize()?.as_os_str().as_encoded_bytes(),
    ));
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
    products_ready: watch::Receiver<Option<PathBuf>>,
    simulator_source: SimulatorSource,
    runtime_root: PathBuf,
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
    let products = target.join("qa/ios-derived-data/Build/Products");
    let run = products.join(label.as_ref()).with_extension("xctestrun");
    let log = File::create(prefix.with_extension("log"))?;
    let host_log = File::create(prefix.with_extension("host.log"))?;
    let temporary = tempfile::Builder::new().prefix("agent-ios-").tempdir()?;
    let root = temporary.path();
    if needs_file_preview_fixtures(&tests) {
        fs::create_dir(root.join("host"))?;
        for entry in fs::read_dir(records.join("file-previews"))? {
            let entry = entry?;
            fs::copy(entry.path(), root.join("host").join(entry.file_name()))?;
        }
    }
    let cwd = std::env::current_dir()?;
    let codex = if without_codex {
        "--without-codex".into()
    } else {
        target.join("debug/agent-codex-fixture").into_os_string()
    };
    let mut command = Command::new(target.join("debug/agent-ui-fixture"));
    command
        .args(args![
            root.join("host"),
            codex,
            root.join("pairing.port"),
            "8000",
            target.join("debug/agent-claude-fixture")
        ])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(host_log.try_clone()?)
        .stderr(host_log);
    if needs_browser_fixture(&tests) {
        command.arg("--prepare-browser");
    }
    let mut host = None;
    let result: Result<_> = async {
            let create = match simulator_source {
                SimulatorSource::Template(template) => args![vec; "xcrun", "simctl", "clone", template, &name],
                SimulatorSource::Runtime(runtime) => args![vec; "xcrun", "simctl", "create", &name, SIMULATOR_DEVICE_TYPE, runtime],
            };
            let simulator = supervision::run(&create, &cwd, Io::Capture, &cancel, SETUP_TIMEOUT).await?;
            let simulator = std::str::from_utf8(&simulator.stdout)?.trim();
            for (phase, arguments) in [
                ("boot", args![vec; "xcrun", "simctl", "boot", simulator]),
                ("bootstatus", args![vec; "xcrun", "simctl", "bootstatus", simulator, "-b"]),
            ] {
                println!("{label}: {phase}");
                supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, SETUP_TIMEOUT).await
                    .map_err(|error| format!("{label}: {phase} failed: {error}"))?;
            }
            let quiet_started = Instant::now();
            let (stopped, present) = quiet_simulator(simulator, &runtime_root, &cwd, &log, &cancel).await?;
            println!("{label}: stopped {stopped}/{present} unused system services in {:.2}s", quiet_started.elapsed().as_secs_f64());
            println!("{label}: Simulator ready in {:.2}s; waiting for test products", started.elapsed().as_secs_f64());
            let source = wait_for_test_products(products_ready, &cancel).await?;
            host = Some(Child::spawn(command)?);
            let host = host.as_mut().unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while !root.join("pairing.port").is_file() {
                if host.try_wait()?.is_some() { return Err(format!("{label}: UI fixture exited before publishing its port").into()); }
                if Instant::now() >= deadline { return Err(format!("{label}: UI fixture timed out").into()); }
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis(100)) => {}, _ = supervision::cancelled(cancel.clone()) => return Err(supervision::interrupted()) }
            }
            let mut preparation = Vec::new();
            if needs_media_fixtures(&tests) {
                preparation.push(("addmedia", args![vec; "xcrun", "simctl", "addmedia", simulator, "apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png", records.join("attachment-video.mov")]));
            }
            preparation.push(("install", args![vec; "xcrun", "simctl", "install", simulator, products.join("Debug-iphonesimulator/bex.app")]));
            if tests.iter().any(|test| test == "testSimulatorOpensOnlyTheTappedImageAndSavesIt") {
                preparation.push(("photos-add permission", args![vec; "xcrun", "simctl", "privacy", simulator, "grant", "photos-add", "dev.remoteagent.mobile.ios"]));
            }
            for (phase, arguments) in preparation {
                println!("{label}: {phase}");
                supervision::run(&arguments, &cwd, Io::Log(&log), &cancel, SETUP_TIMEOUT).await
                    .map_err(|error| format!("{label}: {phase} failed: {error}"))?;
            }
            if tests.iter().any(|test| test == "testSimulatorCanAttachDownloadAndPrepareAIEdit") {
                let container = supervision::run(&args!["xcrun", "simctl", "get_app_container", simulator, "dev.remoteagent.mobile.ios", "data"], &cwd, Io::Capture, &cancel, SETUP_TIMEOUT).await?;
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
                let mut arguments = args![vec; "nix", "run", ".#maestro", "--", "--device", simulator, "test", "--driver-host-port", driver_port.to_string(), "--config", "apps/mobile/maestro/ios/config.yaml", "--format", "junit", "--output", &report, "--test-output-dir", &bundle, "--env", format!("AGENT_PAIRING_URL={pairing_url}"), "--env", format!("AGENT_IOS_DRIVER_URL=http://127.0.0.1:{driver_port}")];
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
            let mut arguments = args![vec; "xcodebuild", "-derivedDataPath", root.join("xcode"), "-xctestrun", &run, "-destination", format!("platform=iOS Simulator,id={simulator}"), "-parallel-testing-enabled", "NO", "-collect-test-diagnostics", "on-failure", "-resultBundlePath", &bundle];
            arguments.extend(tests.iter().map(|test| format!("-only-testing:UITests/LaunchUITests/{test}").into()));
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
    let shutdown = match host {
        Some(mut host) => host.stop(true, Duration::from_secs(10)).await,
        None => Ok(()),
    };
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

struct TestEnvironment {
    shard: usize,
    shards: usize,
    workers: usize,
    rounds: usize,
}

impl TestEnvironment {
    fn read(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let read = |key, default: &str| -> Result<usize> {
            Ok(lookup(key).unwrap_or_else(|| default.to_owned()).parse()?)
        };
        let shard = read("AGENT_IOS_TEST_SHARD", "0")?;
        let shards = read("AGENT_IOS_TEST_SHARDS", "1")?;
        let workers = read("AGENT_IOS_TEST_WORKERS", "1")?;
        if !(1..=10).contains(&workers) {
            return Err("AGENT_IOS_TEST_WORKERS must be between 1 and 10".into());
        }
        // Each repeated round gets fresh Simulators and Hosts to expose flakes.
        let rounds = read("AGENT_IOS_TEST_ROUNDS", "1")?;
        if !(1..=10).contains(&rounds) {
            return Err("AGENT_IOS_TEST_ROUNDS must be between 1 and 10".into());
        }
        Ok(Self {
            shard,
            shards,
            workers,
            rounds,
        })
    }
}

fn validate_test_names(tests: &[String]) -> Result<()> {
    if tests.is_empty()
        || tests.iter().any(|test| !test.starts_with("testSimulator"))
        || tests.iter().collect::<BTreeSet<_>>().len() != tests.len()
    {
        return Err("Select unique isolated Simulator tests".into());
    }
    Ok(())
}

fn validate_maestro_flows(tests: &[String]) -> Result<()> {
    for test in tests {
        if !Path::new("apps/mobile/maestro/ios")
            .join(test)
            .with_extension("yaml")
            .is_file()
        {
            return Err(format!("No maintained Maestro flow for {test}").into());
        }
    }
    Ok(())
}

async fn build_test_bundle(
    cwd: &Path,
    target: &Path,
    build: &Path,
    driver: TestDriver,
    browser: bool,
    log: &File,
    cancel: &watch::Receiver<bool>,
) -> Result<(PathBuf, f64)> {
    let build_started = Instant::now();
    if driver == TestDriver::Maestro {
        supervision::run(
            &args!["nix", "run", ".#maestro", "--", "--version"],
            cwd,
            Io::Log(log),
            cancel,
            BUILD_TIMEOUT,
        )
        .await?;
    }
    let mut host_build = args![vec;
        "cargo",
        "build",
        "--locked",
        "--package",
        "agent-process",
        "--bin",
        "agent-provider-supervisor",
        "--package",
        "host-fixture",
        "--bin",
        "agent-ui-fixture",
        "--bin",
        "agent-codex-fixture",
        "--bin",
        "agent-claude-fixture"
    ];
    // Full-suite CI caches include the desktop's unified dependency features.
    // Match that graph for focused tests when both caches were restored.
    if browser
        || matches!(
            std::env::var("AGENT_IOS_USE_CACHED_HOST_GRAPH").as_deref(),
            Ok("true")
        )
    {
        host_build.extend(args![
            "--package",
            "agent-desktop",
            "--bin",
            "agent-desktop"
        ]);
    }
    // Each phase reports its duration, so CI logs show where a slow build spent its time.
    for (phase, arguments) in [
        (
            "iOS bindings",
            args![vec; "scripts/build-agent-ios.sh", "simulator"],
        ),
        ("isolated Host fixtures", host_build),
        (
            "SDK runtime",
            args![vec; "node", "scripts/build-claude-sdk.ts", target.join("debug")],
        ),
    ] {
        println!("Building {phase}");
        let phase_started = Instant::now();
        supervision::run(&arguments, cwd, Io::Log(log), cancel, BUILD_TIMEOUT).await?;
        println!(
            "Built {phase} in {:.0}s",
            phase_started.elapsed().as_secs_f64()
        );
    }
    let products = build.join("Build/Products");
    if products.is_dir() {
        for path in xctestrun_files(&products)? {
            fs::remove_file(path)?;
        }
    }
    println!("Building the iOS app and UI test bundle");
    let phase_started = Instant::now();
    let revision = supervision::run(
        &args![
            "git",
            "describe",
            "--always",
            "--dirty",
            "--abbrev=40",
            "--exclude",
            "*"
        ],
        cwd,
        Io::Capture,
        cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let revision = std::str::from_utf8(&revision.stdout)?.trim();
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
            "COMPILER_INDEX_STORE_ENABLE=NO",
            "COMPILATION_CACHE_ENABLE_CACHING=YES",
            "-derivedDataPath",
            &build,
            "CODE_SIGNING_ALLOWED=YES",
            "CODE_SIGN_IDENTITY=-",
            "CODE_SIGNING_REQUIRED=YES",
            format!("AGENT_CARGO_TARGET_DIR={}", target.display()),
            format!("AGENT_BUILD_REVISION={revision}"),
            "build-for-testing"
        ],
        cwd,
        Io::Log(log),
        cancel,
        BUILD_TIMEOUT,
    )
    .await?;
    println!(
        "Built the iOS app and UI test bundle in {:.0}s",
        phase_started.elapsed().as_secs_f64()
    );
    let seconds = build_started.elapsed().as_secs_f64();
    Ok((prebuilt_test_bundle(target, browser)?, seconds))
}

fn test_bundle_paths(browser: bool) -> Vec<&'static str> {
    let mut paths = vec![
        "qa/ios-derived-data/Build/Products",
        "debug/xtask",
        "debug/agent-ui-fixture",
        "debug/agent-codex-fixture",
        "debug/agent-claude-fixture",
        "debug/agent-provider-supervisor",
        "debug/agent-claude-sdk.mjs",
        "debug/Claude-Agent-SDK-LICENSE.md",
    ];
    if browser {
        paths.push("debug/agent-desktop");
    }
    paths
}

fn prebuilt_test_bundle(target: &Path, browser: bool) -> Result<PathBuf> {
    for path in test_bundle_paths(browser) {
        if !target.join(path).exists() {
            return Err(format!("Missing prebuilt iOS test dependency: {path}").into());
        }
    }
    let [run]: [PathBuf; 1] = xctestrun_files(&target.join("qa/ios-derived-data/Build/Products"))?
        .try_into()
        .map_err(|_| "Expected exactly one xctestrun file")?;
    Ok(run)
}

async fn archive_test_bundle(
    target: &Path,
    browser: bool,
    log: &File,
    cancel: &watch::Receiver<bool>,
) -> Result<()> {
    let archive = target.join("qa/ios-e2e.tar.gz");
    // This disposable CI handoff favors compression speed over minimum size.
    let mut arguments = args![vec; "tar", "-I", "gzip -1", "-cf", &archive, "-C", target];
    arguments.extend(test_bundle_paths(browser).into_iter().map(Into::into));
    supervision::run(&arguments, target, Io::Log(log), cancel, BUILD_TIMEOUT).await?;
    println!("Prepared iOS test bundle: {}", archive.display());
    Ok(())
}

fn report_rounds(records: &Path, rounds: usize, test_count: usize) -> Result<()> {
    let mut summaries = Vec::new();
    for entry in fs::read_dir(records)? {
        let path = entry?.path();
        if path.to_string_lossy().ends_with(".summary.json") {
            summaries.push(serde_json::from_slice(&fs::read(path)?)?);
        }
    }
    let counts = failure_counts(&summaries);
    println!(
        "{rounds} rounds of {} tests: {} worker results, {} failing tests",
        test_count,
        summaries.len(),
        counts.len()
    );
    for (test, failed) in &counts {
        println!("  {test}: failed {failed}/{rounds}");
    }
    // CI publishes the same table on the job summary page.
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        let mut table = format!(
            "### iPhone acceptance: {rounds} rounds of {test_count} tests, {} failing\n\n| Test | Failed rounds |\n| --- | --- |\n",
            counts.len()
        );
        for (test, failed) in &counts {
            table.push_str(&format!("| {test} | {failed}/{rounds} |\n"));
        }
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?
            .write_all(table.as_bytes())?;
    }
    Ok(())
}

fn xctestrun_files(products: &Path) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(products)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "xctestrun")
        })
        .collect())
}

async fn target_directory(
    mode: BuildMode,
    cwd: &Path,
    cancel: &watch::Receiver<bool>,
) -> Result<PathBuf> {
    if matches!(mode, BuildMode::Prebuilt | BuildMode::AwaitBuild) {
        // debug/xtask runs prebuilt tests; qa/xtask can receive that archive
        // without overwriting its own running executable.
        return std::env::current_exe()?
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| "Prebuilt iOS runner has no target directory".into());
    }
    let metadata = supervision::run(
        &args!["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd,
        Io::Capture,
        cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    Ok(PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("missing Cargo target directory")?,
    ))
}

async fn prepare_media_fixtures(
    attachments: bool,
    file_previews: bool,
    records: &Path,
    cwd: &Path,
    log: &File,
    cancel: &watch::Receiver<bool>,
) -> Result<()> {
    for (needed, name, seconds) in [
        (attachments, "attachment-video.mov", "1"),
        (file_previews, "preview-movie.mp4", "10"),
    ] {
        if !needed {
            continue;
        }
        supervision::run(
            &args![
                "xcrun",
                "swift",
                "apps/mobile/iosApp/UITests/Fixtures/create-video.swift",
                records.join(name),
                seconds
            ],
            cwd,
            Io::Log(log),
            cancel,
            SETUP_TIMEOUT,
        )
        .await?;
    }
    if file_previews {
        supervision::run(
            &args![
                "xcrun",
                "swift",
                "apps/mobile/iosApp/UITests/Fixtures/create-file-previews.swift",
                records.join("file-previews"),
                records.join("preview-movie.mp4")
            ],
            cwd,
            Io::Log(log),
            cancel,
            SETUP_TIMEOUT,
        )
        .await?;
    }
    Ok(())
}

async fn wait_for_test_products(
    mut products: watch::Receiver<Option<PathBuf>>,
    cancel: &watch::Receiver<bool>,
) -> Result<PathBuf> {
    loop {
        if *cancel.borrow() {
            return Err(supervision::interrupted());
        }
        if let Some(path) = products.borrow_and_update().clone() {
            return Ok(path);
        }
        tokio::select! {
            changed = products.changed() => changed.map_err(|_| "iPhone test build did not become available")?,
            _ = supervision::cancelled(cancel.clone()) => return Err(supervision::interrupted()),
        }
    }
}

async fn receive_test_bundle(
    target: &Path,
    browser: bool,
    cwd: &Path,
    log: &File,
    cancel: &watch::Receiver<bool>,
) -> Result<PathBuf> {
    let archive = target.join("qa/ios-e2e.tar.gz");
    println!("Waiting for the shared iPhone app and Host fixtures");
    supervision::run(
        &args![
            "node",
            "scripts/receive-iphone-artifact.ts",
            "iphone-test-build",
            &archive
        ],
        cwd,
        Io::Log(log),
        cancel,
        BUILD_TIMEOUT,
    )
    .await?;
    supervision::run(
        &args!["tar", "-xpzf", &archive, "-C", target],
        cwd,
        Io::Log(log),
        cancel,
        SETUP_TIMEOUT,
    )
    .await?;
    let source = prebuilt_test_bundle(target, browser)?;
    println!("Received the shared iPhone app and Host fixtures");
    Ok(source)
}

async fn acquire_ios_lock(path: &Path, cancel: &watch::Receiver<bool>) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let mut reported = false;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => {
                if !reported {
                    println!("Waiting for iOS lock: {}", path.display());
                    reported = true;
                }
                tokio::select! { _ = tokio::time::sleep(Duration::from_millis(200)) => {}, _ = supervision::cancelled(cancel.clone()) => return Err(supervision::interrupted()) }
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

pub async fn run(
    tests: Vec<String>,
    without_codex: bool,
    driver: TestDriver,
    build_mode: BuildMode,
) -> Result<()> {
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        return Err("iOS E2E requires an Apple Silicon Mac with Xcode".into());
    }
    validate_test_names(&tests)?;
    let environment = TestEnvironment::read(|key| std::env::var(key).ok())?;
    let tests = partition_tests(&tests, environment.shard, environment.shards)?;
    let workers = environment.workers.min(tests.len());
    if driver == TestDriver::Maestro {
        validate_maestro_flows(&tests)?;
    }
    let cancel = supervision::cancellation();
    let started = Instant::now();
    let cwd = std::env::current_dir()?;
    let target = target_directory(build_mode, &cwd, &cancel).await?;
    let qa = target.join("qa");
    fs::create_dir_all(&qa)?;
    let records = qa.join(format!(
        "Bex-{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    ));
    fs::create_dir(&records)?;
    let build = qa.join("ios-derived-data");
    // Keep products stable until their test consumers have finished.
    let _products = acquire_ios_lock(&qa.join("ios-build.lock"), &cancel).await?;
    let log = File::create(records.join("build.log"))?;
    let browser = needs_browser_fixture(&tests);
    let (test_run, build_seconds) = if build_mode == BuildMode::AwaitBuild {
        (None, 0.0)
    } else {
        let (source, seconds) = if build_mode == BuildMode::Prebuilt {
            println!("Using the prebuilt iOS app and Host fixtures");
            (prebuilt_test_bundle(&target, browser)?, 0.0)
        } else {
            build_test_bundle(&cwd, &target, &build, driver, browser, &log, &cancel).await?
        };
        (Some(source), seconds)
    };
    if build_mode == BuildMode::BuildOnly {
        return archive_test_bundle(&target, browser, &log, &cancel).await;
    }
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
    let state =
        PathBuf::from(std::str::from_utf8(&common.stdout)?.trim_end()).join("agent-ios-e2e");
    fs::create_dir_all(&state)?;
    println!("Acquiring shared iOS test lock");
    let _simulator = acquire_ios_lock(&state.join("ios-e2e.lock"), &cancel).await?;
    let groups = (0..workers)
        .map(|index| partition_tests(&tests, index, workers))
        .collect::<Result<Vec<_>>>()?;
    prepare_media_fixtures(
        needs_media_fixtures(&tests),
        needs_file_preview_fixtures(&tests),
        &records,
        &cwd,
        &log,
        &cancel,
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
    let (runtime, runtime_root) = runtime(&serde_json::from_slice(&runtimes.stdout)?)?;
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
    println!(
        "Build: {build_seconds:.2}s; running {} tests on {workers} isolated pairs; records: {}",
        tests.len(),
        records.display()
    );
    let (products_sender, products_ready) = watch::channel(test_run);
    let reception = async {
        let sender = products_sender;
        if build_mode == BuildMode::AwaitBuild {
            let source = receive_test_bundle(&target, browser, &cwd, &log, &cancel).await?;
            sender.send(Some(source))?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    };
    let execution = async {
        let mut results = Vec::new();
        let mut failure = None;
        for round in 1..=environment.rounds {
            // A failed shared build closes the channel. Further fresh rounds
            // cannot execute tests, and reception retains the producer's error.
            if products_ready.has_changed().is_err() && products_ready.borrow().is_none() {
                break;
            }
            let mut pending = JoinSet::new();
            for (index, tests) in groups.iter().cloned().enumerate() {
                let label = if environment.rounds == 1 {
                    format!("worker-{}", index + 1)
                } else {
                    format!("worker-{}-round-{round}", index + 1)
                };
                pending.spawn(worker(
                    tests,
                    target.clone(),
                    products_ready.clone(),
                    simulator_source.clone(),
                    runtime_root.clone(),
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
                        if environment.rounds > 1 {
                            eprintln!("Round {round}/{}: {error}", environment.rounds);
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
        if environment.rounds > 1 {
            report_rounds(&records, environment.rounds, tests.len())?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(results)
    };
    let (received, executed) = tokio::join!(reception, execution);
    received?;
    let results = executed?;
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

    const BUILD_TEST_MODE: &str = "AGENT_IOS_BUILD_TEST_MODE";
    const BUILD_TEST_ROOT: &str = "AGENT_IOS_BUILD_TEST_ROOT";

    #[tokio::test]
    async fn prebuilt_target_needs_no_cargo_while_builds_respect_project_configuration() {
        const ROOT: &str = "AGENT_IOS_TARGET_TEST_ROOT";
        const MODE: &str = "AGENT_IOS_TARGET_TEST_MODE";
        if let Ok(mode) = std::env::var(MODE) {
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            let mode = match mode.as_str() {
                "prebuilt" => BuildMode::Prebuilt,
                "await" => BuildMode::AwaitBuild,
                "build-only" => BuildMode::BuildOnly,
                "build" => BuildMode::BuildAndTest,
                _ => panic!("unexpected test mode"),
            };
            let (_sender, cancel) = watch::channel(false);
            let target = target_directory(mode, &root, &cancel).await.unwrap();
            let expected = if matches!(mode, BuildMode::Prebuilt | BuildMode::AwaitBuild) {
                root.join("shared-build")
            } else {
                root.join("custom-cargo-target")
            };
            assert_eq!(target, expected);
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        for mode in ["prebuilt", "await", "build-only", "build"] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().canonicalize().unwrap();
            let bin = root.join("bin");
            fs::create_dir(&bin).unwrap();
            let cargo = bin.join("cargo");
            fs::write(
                &cargo,
                "#!/bin/sh\n[ \"$1\" = metadata ] || exit 17\nprintf '{\"target_directory\":\"%s/custom-cargo-target\"}\\n' \"$AGENT_IOS_TARGET_TEST_ROOT\"\n",
            )
            .unwrap();
            fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
            let runner = root.join(if mode == "await" {
                "shared-build/qa/xtask"
            } else {
                "shared-build/debug/xtask"
            });
            fs::create_dir_all(runner.parent().unwrap()).unwrap();
            fs::copy(std::env::current_exe().unwrap(), &runner).unwrap();
            let output = Command::new(&runner)
                .args([
                    "--exact",
                    "ios_e2e::tests::prebuilt_target_needs_no_cargo_while_builds_respect_project_configuration",
                    "--nocapture",
                ])
                .env(ROOT, &root)
                .env(MODE, mode)
                .env("PATH", if matches!(mode, "prebuilt" | "await") { Path::new("") } else { &bin })
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[tokio::test]
    async fn test_products_wait_handles_arrival_producer_failure_and_cancellation() {
        let (_cancel_sender, cancel) = watch::channel(false);
        let path = PathBuf::from("shared-products.xctestrun");
        let (sender, ready) = watch::channel(Some(path.clone()));
        drop(sender);
        assert_eq!(wait_for_test_products(ready, &cancel).await.unwrap(), path);

        let (sender, pending) = watch::channel(None);
        let waiting = tokio::spawn({
            let cancel = cancel.clone();
            async move { wait_for_test_products(pending, &cancel).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        sender.send(Some(path.clone())).unwrap();
        assert_eq!(waiting.await.unwrap().unwrap(), path);

        let (sender, pending) = watch::channel(None);
        drop(sender);
        assert!(
            wait_for_test_products(pending, &cancel)
                .await
                .unwrap_err()
                .to_string()
                .contains("did not become available")
        );
        for products in [None, Some(path)] {
            let (_sender, pending) = watch::channel(products);
            let (_cancel_sender, cancelled) = watch::channel(true);
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_test_products(pending, &cancelled),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::Interrupted
            );
        }
    }

    fn build_fixture() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        for directory in [
            "bin",
            "scripts",
            "target/debug",
            "target/qa/ios-derived-data/Build/Products",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("target/debug/xtask"), "test runner").unwrap();
        fs::write(
            root.join("metadata.json"),
            serde_json::to_vec(&json!({"target_directory": root.join("target")})).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("target/qa/ios-derived-data/Build/Products/stale.xctestrun"),
            "old run",
        )
        .unwrap();
        let script = r#"#!/usr/bin/env bash
set -eu
root=$AGENT_IOS_BUILD_TEST_ROOT
printf '%s\n' "${0##*/} $*" >> "$root/commands"
case ${0##*/} in
  git)
    if [[ $1 == rev-parse ]]; then echo "$root/git-common"; else echo fixture-revision; fi ;;
  build-agent-ios.sh) touch "$root/ios-ready" ;;
  cargo)
    if [[ $1 == metadata ]]; then cat "$root/metadata.json"; exit; fi
    [[ $AGENT_IOS_BUILD_TEST_MODE != failure ]] || exit 19
    previous=''
    for argument; do
      if [[ $previous == --bin ]]; then touch "$root/target/debug/$argument"; fi
      previous=$argument
    done ;;
  node) touch "$root/target/debug/agent-claude-sdk.mjs" "$root/target/debug/Claude-Agent-SDK-LICENSE.md" ;;
  xcodebuild)
    test -e "$root/ios-ready"
    touch "$root/target/qa/ios-derived-data/Build/Products/Bex.xctestrun" ;;
  xcrun) touch "$root/simulator-access"; exit 23 ;;
esac
"#;
        for executable in [
            "scripts/build-agent-ios.sh",
            "bin/cargo",
            "bin/node",
            "bin/xcodebuild",
            "bin/xcrun",
            "bin/git",
        ] {
            let path = root.join(executable);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        temporary
    }

    fn build_command(root: &Path, test: &str, mode: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--nocapture"])
            .current_dir(root)
            .env(BUILD_TEST_MODE, mode)
            .env(BUILD_TEST_ROOT, root)
            .env("AGENT_IOS_TEST_SHARD", "0")
            .env("AGENT_IOS_TEST_SHARDS", "1")
            .env("AGENT_IOS_TEST_WORKERS", "1")
            .env("AGENT_IOS_TEST_ROUNDS", "1")
            .env(
                "AGENT_IOS_USE_CACHED_HOST_GRAPH",
                if mode == "cached" { "true" } else { "false" },
            )
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(root.join("bin"))
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            );
        command
    }

    #[tokio::test]
    async fn build_uses_one_host_graph_and_stops_on_failure() {
        if let Some(mode) = std::env::var_os(BUILD_TEST_MODE) {
            let root = PathBuf::from(std::env::var_os(BUILD_TEST_ROOT).unwrap());
            let target = root.join("target");
            let log = File::create(root.join("build.log")).unwrap();
            let (_sender, cancel) = watch::channel(false);
            let result = build_test_bundle(
                &root,
                &target,
                &target.join("qa/ios-derived-data"),
                TestDriver::XCTest,
                mode == "browser",
                &log,
                &cancel,
            )
            .await;
            if mode == "failure" {
                assert!(result.unwrap_err().to_string().contains("cargo failed"));
            } else {
                result.unwrap();
            }
            return;
        }
        for mode in ["host", "browser", "cached", "failure"] {
            let temporary = build_fixture();
            let root = temporary.path().canonicalize().unwrap();
            let output = build_command(
                &root,
                "ios_e2e::tests::build_uses_one_host_graph_and_stops_on_failure",
                mode,
            )
            .output()
            .await
            .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let commands = fs::read_to_string(root.join("commands")).unwrap();
            assert_eq!(
                commands
                    .lines()
                    .filter(|line| line.starts_with("cargo "))
                    .count(),
                1
            );
            let desktop = matches!(mode, "browser" | "cached");
            assert_eq!(commands.contains("--bin agent-desktop"), desktop);
            assert_eq!(commands.contains("xcodebuild "), mode != "failure");
            assert_eq!(
                commands.contains("AGENT_BUILD_REVISION=fixture-revision"),
                mode != "failure"
            );
            assert_eq!(root.join("target/debug/agent-desktop").is_file(), desktop);
        }
    }

    #[tokio::test]
    async fn simulator_preparation_precedes_products_and_failures_delete_only_owned_devices() {
        if let Ok(mode) = std::env::var(BUILD_TEST_MODE) {
            let root = PathBuf::from(std::env::var_os(BUILD_TEST_ROOT).unwrap());
            let records = root.join("records");
            fs::create_dir(&records).unwrap();
            let (sender, products) = watch::channel(None);
            drop(sender);
            let (_sender, cancel) = watch::channel(false);
            let error = worker(
                vec!["testSimulatorExample".into()],
                root.join("target"),
                products,
                SimulatorSource::Runtime("runtime".into()),
                root.join("runtime"),
                records.join("worker-1"),
                false,
                TestDriver::XCTest,
                cancel,
            )
            .await
            .err()
            .unwrap();
            assert!(error.to_string().contains(if mode == "boot-failure" {
                "boot failed"
            } else {
                "did not become available"
            }));
            return;
        }
        for mode in ["missing-products", "boot-failure"] {
            let temporary = build_fixture();
            let root = temporary.path().canonicalize().unwrap();
            fs::write(
                root.join("bin/xcrun"),
                r#"#!/usr/bin/env bash
set -eu
root=$AGENT_IOS_BUILD_TEST_ROOT
printf 'xcrun %s\n' "$*" >> "$root/commands"
case "$2" in
  create) printf '%s' "$3" > "$root/device-name"; echo owned-device ;;
  boot) if [[ $AGENT_IOS_BUILD_TEST_MODE == boot-failure ]]; then exit 17; fi ;;
  list)
    if [[ -e $root/device-name ]]; then
      printf '{"devices":{"runtime":[{"name":"%s","udid":"owned-device"},{"name":"unrelated","udid":"external-device"}]}}\n' "$(cat "$root/device-name")"
    else
      echo '{"devices":{}}'
    fi ;;
  delete) rm "$root/device-name" ;;
esac
"#,
            )
            .unwrap();
            let output = build_command(
                &root,
                "ios_e2e::tests::simulator_preparation_precedes_products_and_failures_delete_only_owned_devices",
                mode,
            )
            .output()
            .await
            .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let commands = fs::read_to_string(root.join("commands")).unwrap();
            assert!(commands.contains("simctl create Bex isolated E2E records-worker-1"));
            assert_eq!(
                commands.contains("simctl bootstatus owned-device -b"),
                mode == "missing-products"
            );
            assert!(commands.contains("simctl delete owned-device"));
            assert!(!commands.contains("simctl delete external-device"));
            assert!(!commands.contains("simctl install"));
            assert!(!root.join("device-name").exists());
        }
    }

    #[tokio::test]
    async fn cancelling_a_lock_wait_keeps_the_owner_and_allows_later_reuse() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("products.lock");
        let owner = File::create(&path).unwrap();
        owner.lock().unwrap();
        let (sender, cancel) = watch::channel(false);
        let cancellation = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            sender.send(true).unwrap();
        });
        let error = tokio::time::timeout(Duration::from_secs(5), acquire_ios_lock(&path, &cancel))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        cancellation.await.unwrap();
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(owner);
        let (_sender, cancel) = watch::channel(false);
        let acquired = acquire_ios_lock(&path, &cancel).await.unwrap();
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(acquired);
        probe.try_lock().unwrap();
    }

    #[cfg(target_os = "macos")]
    async fn wait_for_lock_message(log: &Path, lock: &Path) {
        let expected = format!("Waiting for iOS lock: {}", lock.display());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !fs::read_to_string(log).unwrap().contains(&expected) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Missing {expected}: {}", fs::read_to_string(log).unwrap()));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn builds_guard_their_products_without_waiting_for_another_worktrees_simulator() {
        if let Ok(mode) = std::env::var(BUILD_TEST_MODE) {
            let result = run(
                vec!["testSimulatorUsesNativeProjectDisclosureAndWorkspaceTree".into()],
                false,
                TestDriver::XCTest,
                if mode == "build-only" {
                    BuildMode::BuildOnly
                } else {
                    BuildMode::BuildAndTest
                },
            )
            .await;
            if mode == "build-only" {
                result.unwrap();
            } else {
                assert!(result.unwrap_err().to_string().contains("xcrun failed"));
            }
            return;
        }
        for mode in ["build-only", "test"] {
            let temporary = build_fixture();
            let root = temporary.path().canonicalize().unwrap();
            let state = root.join("git-common/agent-ios-e2e");
            fs::create_dir_all(&state).unwrap();
            let simulator_path = state.join("ios-e2e.lock");
            let simulator = File::create(&simulator_path).unwrap();
            simulator.lock().unwrap();
            let product_path = root.join("target/qa/ios-build.lock");
            let products = (mode == "test").then(|| {
                let file = File::create(&product_path).unwrap();
                file.lock().unwrap();
                file
            });
            let log_path = root.join("child.log");
            let log = File::create(&log_path).unwrap();
            let mut child = build_command(
                &root,
                "ios_e2e::tests::builds_guard_their_products_without_waiting_for_another_worktrees_simulator",
                mode,
            )
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
            if products.is_some() {
                wait_for_lock_message(&log_path, &product_path).await;
                assert!(!root.join("ios-ready").exists());
            }
            drop(products);
            if mode == "test" {
                wait_for_lock_message(&log_path, &simulator_path).await;
                assert!(root.join("ios-ready").exists());
                assert!(!root.join("simulator-access").exists());
                drop(simulator);
            }
            let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .unwrap_or_else(|_| {
                    panic!("{mode} blocked: {}", fs::read_to_string(&log_path).unwrap())
                })
                .unwrap();
            assert!(
                status.success(),
                "{}",
                fs::read_to_string(&log_path).unwrap()
            );
            assert_eq!(root.join("simulator-access").exists(), mode == "test");
            assert_eq!(
                root.join("target/qa/ios-e2e.tar.gz").exists(),
                mode == "build-only"
            );
        }
    }

    #[tokio::test]
    async fn shared_test_bundle_preserves_products_and_executables() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("build");
        let products = target.join("qa/ios-derived-data/Build/Products");
        fs::create_dir_all(&products).unwrap();
        fs::create_dir_all(target.join("debug")).unwrap();
        for name in [
            "xtask",
            "agent-ui-fixture",
            "agent-codex-fixture",
            "agent-claude-fixture",
            "agent-provider-supervisor",
            "agent-desktop",
        ] {
            let path = target.join("debug").join(name);
            fs::write(&path, format!("executable {name}")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(target.join("debug/agent-claude-sdk.mjs"), "SDK runtime").unwrap();
        fs::write(target.join("debug/Claude-Agent-SDK-LICENSE.md"), "license").unwrap();
        fs::write(products.join("Bex.xctestrun"), "test configuration").unwrap();
        let app = products.join("Debug-iphonesimulator/bex.app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("bex"), "iPhone app").unwrap();
        symlink("bex", app.join("app-link")).unwrap();
        let log = File::create(directory.path().join("archive.log")).unwrap();
        let (_sender, cancel) = watch::channel(false);
        for browser in [false, true] {
            archive_test_bundle(&target, browser, &log, &cancel)
                .await
                .unwrap();
            let restored = tempfile::tempdir().unwrap();
            supervision::run(
                &args![
                    "sh",
                    "-c",
                    "umask 077; exec tar \"$@\"",
                    "tar",
                    "-xpzf",
                    target.join("qa/ios-e2e.tar.gz"),
                    "-C",
                    restored.path()
                ],
                directory.path(),
                Io::Log(&log),
                &cancel,
                SETUP_TIMEOUT,
            )
            .await
            .unwrap();
            let restored = restored.path();
            let run = prebuilt_test_bundle(restored, browser).unwrap();
            assert_eq!(fs::read_to_string(run).unwrap(), "test configuration");
            assert_eq!(
                fs::read_to_string(restored.join(
                    "qa/ios-derived-data/Build/Products/Debug-iphonesimulator/bex.app/app-link"
                ))
                .unwrap(),
                "iPhone app"
            );
            assert_eq!(
                fs::metadata(restored.join("debug/xtask"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            assert_eq!(restored.join("debug/agent-desktop").exists(), browser);
            if !browser {
                assert!(
                    prebuilt_test_bundle(restored, true)
                        .unwrap_err()
                        .to_string()
                        .contains("agent-desktop")
                );
            }
            fs::remove_file(restored.join("debug/agent-ui-fixture")).unwrap();
            assert!(
                prebuilt_test_bundle(restored, browser)
                    .unwrap_err()
                    .to_string()
                    .contains("agent-ui-fixture")
            );
        }
        fs::remove_file(products.join("Bex.xctestrun")).unwrap();
        assert!(prebuilt_test_bundle(&target, true).is_err());
        for name in ["first.xctestrun", "second.xctestrun"] {
            fs::write(products.join(name), "configuration").unwrap();
        }
        assert!(prebuilt_test_bundle(&target, true).is_err());
    }

    #[test]
    fn test_environment_parses_defaults_and_bounds_worker_and_round_counts() {
        let defaults = TestEnvironment::read(|_| None).unwrap();
        assert_eq!(
            (
                defaults.shard,
                defaults.shards,
                defaults.workers,
                defaults.rounds
            ),
            (0, 1, 1, 1)
        );
        for key in ["AGENT_IOS_TEST_WORKERS", "AGENT_IOS_TEST_ROUNDS"] {
            for value in ["0", "11", "bad"] {
                assert!(TestEnvironment::read(|name| (name == key).then(|| value.into())).is_err());
            }
            for value in ["1", "10"] {
                assert!(TestEnvironment::read(|name| (name == key).then(|| value.into())).is_ok());
            }
        }
        let selected = TestEnvironment::read(|key| match key {
            "AGENT_IOS_TEST_SHARD" => Some("2".into()),
            "AGENT_IOS_TEST_SHARDS" => Some("3".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!((selected.shard, selected.shards), (2, 3));
    }

    #[test]
    fn invalid_test_names_are_rejected() {
        assert!(validate_test_names(&[]).is_err());
        assert!(validate_test_names(&["other".into()]).is_err());
        assert!(
            validate_test_names(&["testSimulatorOne".into(), "testSimulatorOne".into()]).is_err()
        );
    }

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
                "44 1 100 99 /Devices/{simulator}0/data/Applications/bex.app/bex\n\
                 45 1 100 99 /Devices/{simulator}/data/Applications/bex.app/UITests-Runner\n\
                 46 1 100 99 /Devices/{simulator}/data/Applications/bex.app/BexHelper\n\
                 {pid} 1 100 99 /Devices/{simulator}/data/Applications/bex.app/bex\n"
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
        let xml = br#"<?xml version="1.0"?><plist version="1.0"><dict><key>TestConfigurations</key><array><dict><key>TestTargets</key><array><dict><key>TestBundlePath</key><string>__TESTROOT__/UITests.xctest</string><key>EnvironmentVariables</key><dict><key>UNCHANGED</key><string>value</string></dict><key>IsUITestBundle</key><true/></dict></array></dict></array></dict></plist>"#;
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
                environment.remove("AGENT_PAIRING_URL").unwrap().as_string(),
                Some(url.as_str())
            );
            assert_eq!(
                environment
                    .remove("AGENT_TERMINAL_QUERY_PROBE")
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
        let entry = |version: &str, platform, available| json!({"version": version, "platform": platform, "isAvailable": available, "identifier": version, "runtimeRoot": format!("/runtimes/{version}")});
        assert_eq!(runtime(&json!({"runtimes": [entry("26.9", "iOS", true), entry("26.10", "iOS", true), entry("26.11", "iOS", false), entry("27.1", "iOS", true), entry("26.12", "tvOS", true)]})).unwrap(), ("26.10".to_owned(), PathBuf::from("/runtimes/26.10")));
        assert!(runtime(&json!({"runtimes": [entry("26.1", "iOS", false)]})).is_err());
    }

    #[test]
    fn only_photo_picker_tests_need_seeded_media() {
        let conversation = vec![
            "testSimulatorOpensLongInterruptedHistoryAtLatestMessage".to_owned(),
            "testSimulatorOpensOnlyTheTappedImageAndSavesIt".to_owned(),
            "testSimulatorCanAttachDownloadAndPrepareAIEdit".to_owned(),
            "testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening".to_owned(),
        ];
        assert!(!needs_media_fixtures(&conversation));
        for test in [
            "testSimulatorCanAddASecondPhoto",
            "testSimulatorCanAttachPhotosAndVideos",
            "testSimulatorRetriesPhotoUploadAfterStorageRecovery",
        ] {
            let mut mixed = conversation.clone();
            mixed.push(test.to_owned());
            assert!(needs_media_fixtures(&mixed));
        }
    }

    #[test]
    fn only_file_link_tests_need_native_preview_files() {
        let mut tests = vec![
            "testSimulatorCanAttachPhotosAndVideos".to_owned(),
            "testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopeningExtra".to_owned(),
        ];
        assert!(!needs_file_preview_fixtures(&[]));
        assert!(!needs_file_preview_fixtures(&tests));
        tests.push("testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening".to_owned());
        assert!(needs_file_preview_fixtures(&tests));
    }

    #[test]
    fn only_browser_tests_prepare_native_renderer_before_booting_simulator() {
        let mut tests = vec![
            "testSimulatorOpensOnlyTheTappedImageAndSavesIt".to_owned(),
            "testSimulatorRendersMarkdownTableAndReopensIt".to_owned(),
        ];
        assert!(!needs_browser_fixture(&[]));
        assert!(!needs_browser_fixture(&tests));
        tests.push("testSimulatorBrowserUsesOneSharedImportMenuExtra".to_owned());
        assert!(!needs_browser_fixture(&tests));
        tests.push("testSimulatorBrowserUsesOneSharedImportMenu".to_owned());
        assert!(needs_browser_fixture(&tests));
        tests.reverse();
        assert!(needs_browser_fixture(&tests));
    }

    #[test]
    fn quiet_simulator_keeps_services_used_by_the_app_and_ui_tests() {
        let services = IDLE_SIMULATOR_SERVICES
            .iter()
            .map(|path| service_label(path))
            .collect::<BTreeSet<_>>();
        assert_eq!(services.len(), IDLE_SIMULATOR_SERVICES.len());
        // Each entry is a launchd plist under the runtime root, named by its label.
        assert!(
            IDLE_SIMULATOR_SERVICES.iter().all(|path| {
                path.starts_with("System/Library/") && service_label(path) != *path
            })
        );
        for required in [
            "com.apple.SpringBoard",
            "com.apple.springboard",
            "com.apple.backboardd",
            "com.apple.runningboardd",
            "com.apple.AccessibilityUIServer",
            "com.apple.assetsd",
            "com.apple.nsurlsessiond",
            "com.apple.tccd",
            "com.apple.pluginkit.pkd",
            "com.apple.sharingd",
            "com.apple.mediaremoted",
            "com.apple.corespeechd",
        ] {
            assert!(
                !services.contains(required) && !IDLE_SIMULATOR_APPS.contains(&required),
                "{required} must keep running"
            );
        }
    }

    #[tokio::test]
    async fn simulator_cleanup_overlaps_independent_stops_and_checks_the_final_state() {
        const ROOT: &str = "AGENT_IOS_QUIET_TEST_ROOT";
        if let Some(root) = std::env::var_os(ROOT) {
            let root = PathBuf::from(root);
            let (_sender, cancel) = watch::channel(false);
            let log = File::create(root.join("cleanup.log")).unwrap();
            let result = quiet_simulator("isolated-fixture", &root, &root, &log, &cancel)
                .await
                .unwrap();
            assert_eq!(result, (1, 2));
            assert!(!root.join("timed-out").exists());
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        for path in &IDLE_SIMULATOR_SERVICES[..2] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "fixture service").unwrap();
        }
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let program = bin.join("xcrun");
        fs::write(
            &program,
            r#"#!/usr/bin/env bash
set -eu
root=$AGENT_IOS_QUIET_TEST_ROOT
[[ "$1 $3" == 'simctl isolated-fixture' ]] || exit 17
wait_for_apps() {
  for ((i=0;i<200;i++)); do
    ready=true
    for app in $AGENT_IOS_QUIET_TEST_APPS; do
      [[ -f "$root/$app.started" ]] || ready=false
    done
    $ready && return
    sleep .01
  done
  touch "$root/timed-out"
  exit 23
}
if [[ $2 == terminate ]]; then
  touch "$root/$4.started"
  wait_for_apps
  touch "$root/$4.finished"
  exit 3 # An app that already exited does not invalidate cleanup.
elif [[ $5 == bootout ]]; then
  wait_for_apps
  touch "$root/${7##*/}.finished"
  exit 7 # Final state, rather than bootout's exit code, determines the count.
else
  for app in $AGENT_IOS_QUIET_TEST_APPS; do test -f "$root/$app.finished"; done
  for path in "$root"/System/Library/LaunchDaemons/*; do
    test -f "$root/${path##*/}.finished"
  done
  printf '42 - com.apple.Maps.mapssyncd\n'
fi
"#,
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ios_e2e::tests::simulator_cleanup_overlaps_independent_stops_and_checks_the_final_state",
                "--nocapture",
            ])
            .env(ROOT, &root)
            .env("AGENT_IOS_QUIET_TEST_APPS", IDLE_SIMULATOR_APPS.join(" "))
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(bin)
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            )
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(root.join("cleanup.log")).unwrap()
        );
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
