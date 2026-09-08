import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorApprovalEditorAndDraftSurviveReconnect() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] Verify approvals")
        let accept = app.buttons["request.accept"]
        XCTAssertTrue(accept.waitForExistence(timeout: 15))
        accept.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))

        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Keep this draft")
        openFiles(app)
        let file = app.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitForExistence(timeout: 10)); file.tap()
        let editor = app.textViews["file.editor"]
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        editor.tap(); editor.typeText("Saved from iPhone\n")
        app.buttons["file.save"].tap()
        let saved = expectation(for: NSPredicate(format: "isEnabled == false"), evaluatedWith: app.buttons["file.save"])
        wait(for: [saved], timeout: 10)
        app.buttons["file.close"].tap()
        file.tap()
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        XCTAssertTrue((editor.value as? String ?? "").contains("Saved from iPhone"))
        app.buttons["file.close"].tap()
        app.buttons["files.diff"].tap()
        XCTAssertTrue(app.staticTexts["作業中の差分"].waitForExistence(timeout: 10))
        app.buttons["files.diff.close"].tap()
        app.buttons["files.close"].tap()
        XCTAssertEqual(message.value as? String, "Keep this draft")

        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        let restored = reopened.descendants(matching: .any)["task.message"]
        XCTAssertTrue(restored.waitForExistence(timeout: 10))
        XCTAssertEqual(restored.value as? String, "Keep this draft")
    }

    func testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] [deferred-steer] Hold this turn")
        XCTAssertTrue(app.buttons["request.accept"].waitForExistence(timeout: 15))
        let command = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        let message = app.textFields["task.message"]
        message.tap(); message.typeText("Show this additional input immediately")
        app.buttons["task.send"].tap()
        let sent = app.staticTexts["Show this additional input immediately"]
        XCTAssertTrue(sent.waitForExistence(timeout: 2), "Accepted input must be visible while its native echo is held")
        XCTAssertTrue(sent.isHittable, "Additional input must remain visible at the latest position")
        XCTAssertGreaterThan(sent.frame.minY, command.frame.minY, "Additional input was moved above the preceding work")
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-steer-recorded-").exists)
        captureScreen(app, named: "Accepted additional input before native echo")
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-steer-recorded-").waitForExistence(timeout: 10))
        XCTAssertEqual(
            app.staticTexts.matching(NSPredicate(format: "label == %@", "Show this additional input immediately"))
                .count,
            1
        )
        prefixedButton(app, prefix: "turn.interrupt.").tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch
            .waitForExistence(timeout: 15))
        XCTAssertEqual(
            app.staticTexts.matching(NSPredicate(format: "label == %@", "Show this additional input immediately"))
                .count,
            1
        )
    }

    func testSimulatorCanSteerAndStopAnActiveTurn() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] Hold this turn")
        XCTAssertTrue(app.buttons["request.accept"].waitForExistence(timeout: 15))
        captureScreen(app, named: "Active conversation text and tool labels")
        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Change the requested approach")
        app.buttons["task.send"].tap()
        XCTAssertTrue(app.staticTexts["Change the requested approach"].waitForExistence(timeout: 2))
        let stop = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(stop.waitForExistence(timeout: 5)); stop.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch
            .waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["request.accept"].exists)
        XCTAssertFalse(stop.exists)
    }
}
