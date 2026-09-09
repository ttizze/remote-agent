use crate::command::{self, ChildProcess};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;
use xtask::{Result, pairing::PairingServer, repository_root};

const DEFAULT_TESTS: &[&str] = &[
    "testSimulatorEditsHostWorktreeSettingsFromTaskMenu",
    "testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject",
    "testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject",
    "testSimulatorPaginatesRecentProjectsAndUnassignedChats",
    "testSimulatorShowsWorkspaceConversationInsideItsProject",
    "testSimulatorFetchesNewTaskWhenReturningToList",
    "testSimulatorFetchesNewTaskAfterForeground",
    "testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground",
    "testSimulatorUpdatesAnOpenConversationFromAnotherClient",
    "testSimulatorReviewsTheOpenSessionsWorktree",
    "testSimulatorStartsOnListAndPreservesDetailOnForeground",
    "testSimulatorSwitchesCodexAccountsAndForksConversation",
    "testSimulatorReturnsToListWithNativeEdgeSwipeAndRetainsDrafts",
    "testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation",
    "testSimulatorUsesNativeHostNavigationAndPairingDismissal",
    "testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation",
    "testSimulatorCanStartAConversationInAProject",
    "testSimulatorMarksUnseenCompletionUntilOpened",
    "testSimulatorDictationPermissionDenialPreservesDraftAndSend",
    "testSimulatorDictationContinuesPastThirtySecondsAndReachesHost",
    "testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap",
    "testSimulatorApprovalEditorAndDraftSurviveReconnect",
    "testSimulatorKeepsInputRequestVisibleUntilResolved",
    "testSimulatorShowsRetryingStreamErrorThenRecovers",
    "testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError",
    "testSimulatorKeepsInterruptedWorkCollapsed",
    "testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems",
    "testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory",
    "testSimulatorReopensCompletedHistoryCollapsed",
    "testSimulatorKeepsEarlierAnswersBetweenFollowupsWhenReopening",
    "testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText",
    "testSimulatorOpensLongInterruptedHistoryAtLatestMessage",
    "testSimulatorKeepsSmallOlderScrollDuringLiveUpdate",
    "testSimulatorCanSteerAndStopAnActiveTurn",
    "testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt",
    "testSimulatorCanAttachDownloadAndPrepareAIEdit",
    "testSimulatorCanAddASecondPhoto",
    "testSimulatorCanAttachPhotosAndVideos",
    "testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening",
    "testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening",
];

struct Simulator {
    id: String,
    removed: bool,
}

impl Simulator {
    async fn remove(&mut self) -> Result<()> {
        let _ = command::status(
            Command::new("xcrun")
                .args(["simctl", "shutdown", &self.id])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .await;
        command::run(Command::new("xcrun").args(["simctl", "delete", &self.id])).await?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        if !self.removed {
            for action in ["shutdown", "delete"] {
                let _ = std::process::Command::new("xcrun")
                    .args(["simctl", action, &self.id])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

pub async fn run(arguments: &[String]) -> Result<()> {
    let tests: Vec<_> = if arguments.is_empty() {
        DEFAULT_TESTS.to_vec()
    } else {
        arguments.iter().map(String::as_str).collect()
    };
    if tests.iter().any(|test| !test.starts_with("testSimulator")) {
        return Err("Only isolated simulator tests are allowed".into());
    }
    if (std::env::consts::OS, std::env::consts::ARCH) != ("macos", "aarch64") {
        return Err("iOS E2E requires an Apple Silicon Mac with Xcode".into());
    }
    let target = command::target_directory().await?;
    let qa = target.join("qa");
    fs::create_dir_all(&qa)?;
    // Each run owns its build products and test-run configuration. Concurrent
    // invocations must never overwrite another fixture's pairing URL.
    let build = tempfile::Builder::new()
        .prefix("ios-build-")
        .tempdir_in(&qa)?;
    let fixture = tempfile::Builder::new()
        .prefix("bex-ios.")
        .tempdir_in("/tmp")?;
    command::run(Command::new("./gradlew").args([
        ":apps:mobile:iosSimulatorArm64Test",
        ":apps:mobile:linkDebugFrameworkIosSimulatorArm64",
        "--console=plain",
    ]))
    .await?;
    command::run(command::cargo().args([
        "build",
        "--locked",
        "--package",
        "xtask",
        "--bin",
        "bex-ui-fixture",
        "--bin",
        "bex-codex-fixture",
    ]))
    .await?;
    let log = fs::File::create(fixture.path().join("host.log"))?;
    let mut host = ChildProcess::spawn(
        Command::new(target.join("debug/bex-ui-fixture"))
            .arg(fixture.path().join("host"))
            .arg(target.join("debug/bex-codex-fixture"))
            .arg("8000")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log),
    )?;
    let state = fixture.path().join("host/state");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !state.join("host.ticket").exists() || !state.join("local.key").exists() {
        if host.exited()? {
            return Err(
                "UI fixture exited before publishing its endpoint and local identity".into(),
            );
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Host did not publish its endpoint and local identity".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let pairing = PairingServer::start(&state, 0)?;
    let runtimes: Value = serde_json::from_slice(
        &command::output(Command::new("xcrun").args(["simctl", "list", "runtimes", "-j"])).await?,
    )?;
    let runtime = runtimes["runtimes"]
        .as_array()
        .ok_or("Simulator runtime list is missing")?
        .iter()
        .find(|runtime| runtime["isAvailable"] == true && runtime["platform"] == "iOS")
        .and_then(|runtime| runtime["identifier"].as_str())
        .ok_or("No available iOS Simulator runtime")?;
    let id = command::output(Command::new("xcrun").args([
        "simctl",
        "create",
        "Bex isolated E2E",
        "com.apple.CoreSimulator.SimDeviceType.iPhone-17",
        runtime,
    ]))
    .await?;
    let mut simulator = Simulator {
        id: std::str::from_utf8(&id)?.trim().to_owned(),
        removed: false,
    };
    command::run(Command::new("xcrun").args(["simctl", "boot", &simulator.id])).await?;
    command::run(Command::new("xcrun").args(["simctl", "bootstatus", &simulator.id, "-b"])).await?;
    let video = fixture.path().join("attachment-video.mov");
    command::run(
        Command::new("xcrun")
            .args([
                "swift",
                "apps/mobile/iosApp/BexUITests/Fixtures/create-video.swift",
            ])
            .arg(&video),
    )
    .await?;
    command::run(
        Command::new("xcrun")
            .args(["simctl", "addmedia", &simulator.id])
            .arg(
                repository_root()
                    .join("apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png"),
            )
            .arg(video),
    )
    .await?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let result_bundle = std::env::var_os("BEX_RELAY_RESULT_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| qa.join(format!("Bex-{stamp}-{}.xcresult", std::process::id())));
    command::run(
        Command::new("xcodebuild")
            .args([
                "-project",
                "apps/mobile/iosApp/Bex.xcodeproj",
                "-scheme",
                "Bex",
                "-sdk",
                "iphonesimulator",
                "-configuration",
                "Debug",
                "-derivedDataPath",
            ])
            .arg(build.path())
            .args([
                "CODE_SIGNING_ALLOWED=YES",
                "CODE_SIGN_IDENTITY=-",
                "CODE_SIGNING_REQUIRED=YES",
                "build-for-testing",
            ]),
    )
    .await?;
    let products = build.path().join("Build/Products");
    command::run(
        Command::new("xcrun")
            .args(["simctl", "install", &simulator.id])
            .arg(products.join("Debug-iphonesimulator/Bex.app")),
    )
    .await?;
    command::run(Command::new("xcrun").args([
        "simctl",
        "privacy",
        &simulator.id,
        "grant",
        "photos-add",
        "dev.remoteagent.mobile.ios",
    ]))
    .await?;
    let container = command::output(Command::new("xcrun").args([
        "simctl",
        "get_app_container",
        &simulator.id,
        "dev.remoteagent.mobile.ios",
        "data",
    ]))
    .await?;
    let documents = Path::new(std::str::from_utf8(&container)?.trim()).join("Documents");
    fs::create_dir_all(&documents)?;
    fs::write(
        documents.join("attachment-fixture.txt"),
        "Isolated attachment upload fixture.\n",
    )?;
    let mut runs = fs::read_dir(&products)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "xctestrun")
        });
    let run = runs
        .next()
        .ok_or("Xcode did not produce an xctestrun file")?;
    if runs.next().is_some() {
        return Err("Xcode produced multiple xctestrun files".into());
    }
    let mut configuration = plist::Value::from_file(&run)?;
    if set_pairing_url(
        &mut configuration,
        &format!("http://127.0.0.1:{}/pairing", pairing.port),
    ) == 0
    {
        return Err("xctestrun contains no test bundle".into());
    }
    configuration.to_file_xml(&run)?;
    let mut test = Command::new("xcodebuild");
    test.arg("-xctestrun")
        .arg(&run)
        .arg("-destination")
        .arg(format!("platform=iOS Simulator,id={}", simulator.id))
        .arg("-resultBundlePath")
        .arg(&result_bundle);
    for method in &tests {
        test.arg(format!(
            "-only-testing:BexUITests/BexLaunchUITests/{method}"
        ));
    }
    let test_status = command::status(test.arg("test-without-building")).await?;
    let summary = command::output(
        Command::new("xcrun")
            .args(["xcresulttool", "get", "test-results", "summary", "--path"])
            .arg(&result_bundle)
            .args(["--format", "json"]),
    )
    .await?;
    fs::write(result_bundle.with_extension("summary.json"), &summary)?;
    pairing.shutdown()?;
    host.stop().await?;
    simulator.remove().await?;
    validate_summary(&serde_json::from_slice(&summary)?, tests.len())?;
    if !test_status.success() {
        return Err(format!("xcodebuild exited with {test_status}").into());
    }
    println!(
        "{} iOS UI tests passed; 0 failed, 0 skipped\n{}",
        tests.len(),
        result_bundle.display()
    );
    Ok(())
}

fn set_pairing_url(value: &mut plist::Value, url: &str) -> usize {
    match value {
        plist::Value::Dictionary(fields) if fields.contains_key("TestBundlePath") => {
            if !fields.contains_key("EnvironmentVariables") {
                fields.insert(
                    "EnvironmentVariables".into(),
                    plist::Dictionary::new().into(),
                );
            }
            let environment = fields.get_mut("EnvironmentVariables").unwrap();
            let Some(environment) = environment.as_dictionary_mut() else {
                return 0;
            };
            environment.insert("BEX_PAIRING_URL".into(), url.into());
            1
        }
        plist::Value::Dictionary(fields) => fields
            .iter_mut()
            .map(|(_, value)| set_pairing_url(value, url))
            .sum(),
        plist::Value::Array(values) => values
            .iter_mut()
            .map(|value| set_pairing_url(value, url))
            .sum(),
        _ => 0,
    }
}

fn validate_summary(summary: &Value, expected: usize) -> Result<()> {
    let passed = summary["passedTests"].as_u64();
    let failed = summary["failedTests"].as_u64();
    let skipped = summary["skippedTests"].as_u64();
    if passed != Some(expected as u64) || failed != Some(0) || skipped != Some(0) {
        return Err(format!("iOS test results did not match {expected} passes and zero failures/skips: passed={passed:?}, failed={failed:?}, skipped={skipped:?}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skipped_missing_and_partial_results_are_not_success() {
        assert!(
            validate_summary(
                &json!({"passedTests":2,"failedTests":0,"skippedTests":0}),
                2
            )
            .is_ok()
        );
        for summary in [
            json!({"passedTests":1,"failedTests":0,"skippedTests":1}),
            json!({"passedTests":1,"failedTests":1,"skippedTests":0}),
            json!({"passedTests":1,"failedTests":0,"skippedTests":0}),
            json!({"passedTests":2}),
            json!({"passedTests":3,"failedTests":0,"skippedTests":0}),
        ] {
            assert!(validate_summary(&summary, 2).is_err());
        }
    }

    #[test]
    fn pairing_configuration_preserves_existing_environment_and_plist_types() {
        let mut target = plist::Dictionary::new();
        target.insert(
            "TestBundlePath".into(),
            "/isolated/BexUITests.xctest".into(),
        );
        target.insert("UnknownData".into(), plist::Value::Data(vec![0, 255]));
        target.insert(
            "EnvironmentVariables".into(),
            plist::Dictionary::from_iter([("KEEP", plist::Value::from("value"))]).into(),
        );
        let mut value: plist::Value =
            plist::Dictionary::from_iter([("BexUITests", plist::Value::from(target))]).into();
        assert_eq!(
            set_pairing_url(&mut value, "http://127.0.0.1:1234/pairing"),
            1
        );
        let target = value.as_dictionary().unwrap()["BexUITests"]
            .as_dictionary()
            .unwrap();
        let environment = target["EnvironmentVariables"].as_dictionary().unwrap();
        assert_eq!(environment["KEEP"].as_string(), Some("value"));
        assert_eq!(
            environment["BEX_PAIRING_URL"].as_string(),
            Some("http://127.0.0.1:1234/pairing")
        );
        assert_eq!(target["UnknownData"].as_data(), Some([0, 255].as_slice()));
    }
}
