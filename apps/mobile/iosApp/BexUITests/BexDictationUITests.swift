import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testLaunchKeepsPairingScreenAlive() {
        let app = XCUIApplication()
        app.launch()

        let pairingTitle = app.staticTexts["PCとペアリング"]
        XCTAssertTrue(pairingTitle.waitForExistence(timeout: 10))

        let stayedAlive = XCTWaiter.wait(
            for: [XCTestExpectation(description: "observe process stability")],
            timeout: 2
        )
        XCTAssertEqual(stayedAlive, .timedOut)
        XCTAssertEqual(app.state, .runningForeground)
        XCTAssertTrue(pairingTitle.exists)
    }

    func testSimulatorDictationContinuesPastThirtySecondsAndReachesHost() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        XCUIApplication().resetAuthorizationStatus(for: .microphone)
        let app = try connectedSimulatorApp()
        addTeardownBlock { _ = try self.simulatorFixture("dictation/restore-account") }
        try simulatorFixture("dictation/no-account")
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap(); prompt.typeText("Keep this draft after transcription failure")
        let microphone = app.buttons["dictation.toggle"]
        microphone.tap()
        let systemAlert = XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.firstMatch
        if systemAlert.waitForExistence(timeout: 5) {
            let allow = systemAlert.buttons.matching(NSPredicate(format: "label IN %@", ["Allow", "OK", "許可", "許可する"]))
                .firstMatch
            XCTAssertTrue(allow.exists); allow.tap()
        }
        let recording = app.staticTexts["dictation.recording"]
        XCTAssertTrue(recording.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["dictation.cancel"].exists)
        XCTAssertEqual(recording.label, "録音中")
        let stoppedEarly = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: recording
        )
        XCTAssertEqual(XCTWaiter.wait(for: [stoppedEarly], timeout: 32), .timedOut,
                       "Recording must continue until Stop or Send is pressed")
        captureScreen(app, named: "Recording after 32 seconds with Stop and Send only")
        app.buttons["task.send"].tap()
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(notice.waitForExistence(timeout: 30))
        // The isolated Codex fixture has no account. Reaching this error proves
        // recorded PCM crossed the real mobile bridge, SSH relay and Host route.
        XCTAssertTrue(notice.label.contains("ChatGPT"), notice.label)
        XCTAssertFalse(notice.label.contains("unknown variant"))
        XCTAssertFalse(recording.exists)
        XCTAssertEqual(prompt.value as? String, "Keep this draft after transcription failure")
        XCTAssertTrue(microphone.isEnabled)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
        // Finish this fixture conversation so the next test cannot inherit its draft.
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
    }

    func testSimulatorDictationPermissionDenialPreservesDraftAndSend() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        XCUIApplication().resetAuthorizationStatus(for: .microphone)
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap(); prompt.typeText("Keep this draft after microphone denial")
        let microphone = app.buttons["dictation.toggle"]
        XCTAssertTrue(microphone.exists); microphone.tap()
        let systemAlert = XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.firstMatch
        if systemAlert.waitForExistence(timeout: 5) {
            let deny = systemAlert.buttons.matching(NSPredicate(
                format: "label IN %@",
                ["Don't Allow", "Don’t Allow", "許可しない"]
            )).firstMatch
            XCTAssertTrue(deny.exists, "The microphone permission dialog has no recognized deny action")
            deny.tap()
        }
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(notice.waitForExistence(timeout: 10))
        XCTAssertTrue(notice.label.contains("マイク"))
        XCTAssertEqual(prompt.value as? String, "Keep this draft after microphone denial")
        XCTAssertFalse(app.staticTexts["dictation.recording"].exists)
        XCTAssertFalse(app.buttons["dictation.cancel"].exists)
        XCTAssertTrue(microphone.isEnabled)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
        captureScreen(app, named: "Dictation permission denied with draft retained")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
    }
}
