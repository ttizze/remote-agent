import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorShowsHistoryLoadingBeforeHostReplies() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("external-conversation")
        addTeardownBlock { _ = try self.simulatorFixture("release-history-reads") }
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.buttons["tasks.row.codex:fixture-external-thread"]
        XCTAssertTrue(row.waitUntilExists(timeout: 15))
        try simulatorFixture("hold-history-reads")
        row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["task.loading"].waitUntilExists(timeout: 2),
                      "Navigation must finish while the Host is still holding the history response")
        XCTAssertFalse(app.descendants(matching: .any)["task.empty"].exists)
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        XCTAssertFalse(app.buttons["task.attach"].isEnabled)
        try simulatorFixture("release-history-reads")
        let firstAnswer = app.descendants(matching: .any)["item.answer-fixture-external-thread"]
        XCTAssertTrue(firstAnswer.waitUntilExists(timeout: 15))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "History loading transitions to the native conversation")
    }

    func testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft() throws {
        let app = try connectedSimulatorApp()
        let identifier = try simulatorTaskRowIdentifier("background-task")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.buttons[identifier]
        XCTAssertTrue(row.waitUntilExists(timeout: 15))
        try simulatorFixture("background-reply")
        try simulatorFixture("fail-next-history-read")
        row.tap()
        let retry = app.buttons["task.retry"]
        XCTAssertTrue(retry.waitUntilExists(timeout: 10))
        XCTAssertTrue(app.staticTexts["notice"].exists)
        XCTAssertFalse(app.descendants(matching: .any)["task.loading"].exists)
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("Keep this failed-open draft")
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        retry.tap()
        let answer = app.descendants(matching: .any)["item.fixture-external-final"]
        XCTAssertTrue(answer.waitUntilExists(timeout: 15))
        XCTAssertEqual(composer.value as? String, "Keep this failed-open draft")
        XCTAssertFalse(retry.exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        XCTAssertTrue(answer.waitUntilExists(timeout: 10))
        XCTAssertEqual(composer.value as? String, "Keep this failed-open draft")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Failed task open recovers with its draft retained")
    }

    func testSimulatorReconnectClearsHistoryFailureAndPreservesDraft() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Establish a conversation before retry")
        let firstAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(firstAnswer.waitUntilExists(timeout: 25))
        let firstAnswerID = firstAnswer.identifier
        let secondAnswerID = String(firstAnswerID.dropLast()) + "2"
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("Keep this retry draft")
        XCUIDevice.shared.press(.home)
        try simulatorFixture("fail-next-history-read")
        app.activate()
        let banner = app.descendants(matching: .any)["connection.error"]
        let progress = app.descendants(matching: .any)["connection.progress"]
        let settled = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: progress)
        wait(for: [settled], timeout: 20)
        XCTAssertFalse(banner.exists)
        XCTAssertEqual(composer.value as? String, "Keep this retry draft")
        XCUIDevice.shared.press(.home)
        app.activate()
        XCTAssertEqual(composer.value as? String, "Keep this retry draft")
        let send = app.buttons["task.send"]
        let ready = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        wait(for: [ready], timeout: 15)
        send.tap()
        XCTAssertTrue(app.descendants(matching: .any)[secondAnswerID].waitUntilExists(timeout: 25))
        XCTAssertTrue(app.staticTexts["Keep this retry draft"].exists)
        XCTAssertFalse(banner.exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        XCTAssertNotEqual(composer.value as? String, "Keep this retry draft")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let number = firstAnswerID.replacingOccurrences(of: "item.fixture-final-", with: "")
            .split(separator: "-")[0]
        let row = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(number)")]
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[secondAnswerID].waitUntilExists(timeout: 20))
        XCTAssertFalse(banner.exists)
        captureScreen(app, named: "Reconnect clears history error and preserves submission")
    }

    func testSimulatorUpdatesAnOpenConversationFromAnotherClient() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("external-conversation")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.descendants(matching: .any)["tasks.row.codex:fixture-external-thread"]
        XCTAssertTrue(row.waitUntilExists(timeout: 15)); row.tap()
        XCTAssertTrue(
            app.descendants(matching: .any)["item.answer-fixture-external-thread"].waitUntilExists(timeout: 15),
            "The native paginated conversation must open before another Bex client sends input"
        )
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitUntilExists(timeout: 10)); composer.tap()
        composer.typeText("Keep this unsent draft")
        try simulatorFixture("client-reply")
        XCTAssertTrue(app.textViews["シミュレータで完了しました。"].waitUntilExists(timeout: 30),
                      "The open conversation did not receive the other Bex client's live reply")
        XCTAssertEqual(composer.value as? String, "Keep this unsent draft")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Another Bex client updates the conversation with draft retained")
    }

    func testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Open before background update")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 25))
        let detail = app.descendants(matching: .any)["task.detail"]
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitUntilExists(timeout: 10))
        composer.tap(); composer.typeText("Keep this foreground draft")
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 5))
        try simulatorFixture("background-reply")
        app.activate()
        XCTAssertTrue(detail.waitUntilExists(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["item.fixture-external-final"].waitUntilExists(timeout: 20),
                      "Foreground return did not fetch the conversation changed by another client")
        XCTAssertEqual(composer.value as? String, "Keep this foreground draft")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Latest conversation fetched on foreground")
    }

    func testSimulatorFetchesNewTaskAfterForeground() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        XCUIDevice.shared.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 5))
        let identifier = try simulatorTaskRowIdentifier("background-task")
        app.activate()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)[identifier].waitUntilExists(timeout: 20),
                      "Foreground return did not fetch the conversation created by another client")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        let project = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.project."))
            .firstMatch
        XCTAssertTrue(project.waitUntilExists(timeout: 10)); project.tap()
        let row = app.descendants(matching: .any)[identifier]
        let hidden = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: row)
        wait(for: [hidden], timeout: 10)
        project.tap()
        XCTAssertTrue(row.waitUntilExists(timeout: 10))
        captureScreen(app, named: "New conversation fetched on foreground")
    }

    func testSimulatorFetchesNewTaskWhenReturningToList() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Keep another conversation open")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 25))
        let identifier = try simulatorTaskRowIdentifier("background-task")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)[identifier].waitUntilExists(timeout: 20),
                      "Returning to the list did not fetch the conversation created by another client")
        captureScreen(app, named: "New conversation fetched when returning to the list")
    }

    func testSimulatorMarksUnseenCompletionUntilOpened() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "value == %@", "完了・未確認")).firstMatch.exists)
        try startSimulatorConversation(app, promptText: "[success] Notify when this task finishes")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.codex:bex-task:")
        XCTAssertTrue(row.waitUntilExists(timeout: 10))
        let running = expectation(for: NSPredicate(format: "value == %@", "実行中"), evaluatedWith: row)
        wait(for: [running], timeout: 10)
        XCTAssertEqual(row.value as? String, "実行中")
        captureScreen(app, named: "Task running in list")
        let completed = expectation(for: NSPredicate(format: "value == %@", "完了・未確認"), evaluatedWith: row)
        wait(for: [completed], timeout: 25)
        captureScreen(app, named: "White dot for unseen completion")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        XCTAssertTrue(row.waitUntilExists(timeout: 20))
        XCTAssertEqual(row.value as? String, "完了・未確認")
        row.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitUntilExists(timeout: 10))
        XCTAssertEqual(row.value as? String, "")
        captureScreen(app, named: "Completion dot cleared after opening")
    }

    func testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] [deferred-steer] Hold this turn")
        XCTAssertTrue(app.buttons["request.accept"].waitUntilExists(timeout: 15))
        let command = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        let message = app.textFields["task.message"]
        message.tap(); message.typeText("Show this additional input immediately")
        app.buttons["task.send"].tap()
        let sentMessages = app.textViews.matching(NSPredicate(
            format: "identifier == %@ AND value == %@", "message.user-text", "Show this additional input immediately"
        ))
        let sent = sentMessages.firstMatch
        XCTAssertTrue(sent.waitUntilExists(timeout: 2), "Accepted input must be visible while its native echo is held")
        XCTAssertTrue(sent.isHittable, "Additional input must remain visible at the latest position")
        XCTAssertGreaterThan(sent.frame.minY, command.frame.minY, "Additional input was moved above the preceding work")
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-steer-recorded-").exists)
        captureScreen(app, named: "Accepted additional input before native echo")
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-steer-recorded-").waitUntilExists(timeout: 10))
        XCTAssertEqual(
            sentMessages.count,
            1
        )
        prefixedButton(app, prefix: "turn.interrupt.").tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch
            .waitUntilExists(timeout: 15))
        XCTAssertEqual(
            sentMessages.count,
            1
        )
        XCTAssertFalse(app.buttons["request.accept"].exists)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        command.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitUntilExists(timeout: 5))
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
    }
}
