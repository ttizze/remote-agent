import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorDictationContinuesPastThirtySecondsAndReachesHost() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        try simulatorFixture("auth-token/unavailable")
        addTeardownBlock { _ = try self.simulatorFixture("auth-token/reset") }
        XCUIApplication().resetAuthorizationStatus(for: .microphone)
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap(); prompt.typeText("Keep this draft after transcription failure")
        let microphone = app.buttons["dictation.toggle"]
        microphone.tap()
        respondToMicrophonePermission(allow: true)
        let recording = app.descendants(matching: .any)["dictation.recording"]
        XCTAssertTrue(recording.waitForExistence(timeout: 10))
        let cancel = app.buttons["dictation.cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        cancel.tap()
        XCTAssertTrue(prompt.waitForExistence(timeout: 5))
        XCTAssertEqual(prompt.value as? String, "Keep this draft after transcription failure")
        XCTAssertFalse(app.descendants(matching: .any)["dictation.recording"].exists)
        microphone.tap()
        XCTAssertTrue(recording.waitForExistence(timeout: 10))
        XCTAssertTrue(cancel.exists); XCTAssertFalse(prompt.exists)
        XCTAssertEqual(recording.label, "録音中")
        let stoppedEarly = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: recording
        )
        XCTAssertEqual(XCTWaiter.wait(for: [stoppedEarly], timeout: 32), .timedOut,
                       "Recording must continue until Stop or Send is pressed")
        captureScreen(app, named: "Inline waveform after 32 seconds")
        app.buttons["task.send"].tap()
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(notice.waitForExistence(timeout: 30))
        // The isolated provider has no bearer token for this test. Reaching this
        // error proves recorded PCM crossed UniFFI, iroh and the Host route.
        XCTAssertTrue(notice.label.contains("ChatGPT"), notice.label)
        XCTAssertFalse(notice.label.contains("unknown variant"))
        assertRecoveredDictationDraft(app, text: "Keep this draft after transcription failure")
        microphone.tap()
        XCTAssertTrue(recording.waitForExistence(timeout: 10))
        XCTAssertFalse(notice.exists)
        microphone.tap()
        XCTAssertTrue(notice.waitForExistence(timeout: 30))
        XCTAssertTrue(notice.label.contains("ChatGPT"), notice.label)
        XCTAssertTrue(prompt.waitForExistence(timeout: 5))
        assertRecoveredDictationDraft(app, text: "Keep this draft after transcription failure")
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
        respondToMicrophonePermission(allow: false)
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(notice.waitForExistence(timeout: 10))
        XCTAssertTrue(notice.label.contains("マイク"))
        assertRecoveredDictationDraft(app, text: "Keep this draft after microphone denial")
        XCTAssertFalse(app.buttons["dictation.cancel"].exists)
        captureScreen(app, named: "Dictation permission denied with draft retained")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
    }

    private func assertRecoveredDictationDraft(_ app: XCUIApplication, text: String) {
        XCTAssertFalse(app.descendants(matching: .any)["dictation.recording"].exists)
        XCTAssertEqual(app.descendants(matching: .any)["task.message"].value as? String, text)
        XCTAssertTrue(app.buttons["dictation.toggle"].isEnabled)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
    }

    private func respondToMicrophonePermission(allow: Bool) {
        let alert = XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.firstMatch
        if alert.waitForExistence(timeout: 5) {
            let labels = allow ? ["Allow", "OK", "許可", "許可する"] : ["Don't Allow", "Don’t Allow", "許可しない"]
            let button = alert.buttons.matching(NSPredicate(format: "label IN %@", labels)).firstMatch
            XCTAssertTrue(button.exists, "The microphone permission dialog has no recognized action")
            button.tap()
        }
    }
}
