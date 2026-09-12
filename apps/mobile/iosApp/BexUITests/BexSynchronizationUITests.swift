import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorOpensTasksBeforeHistoryReadFinishes() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("external-conversation")
        addTeardownBlock { _ = try self.simulatorFixture("release-history-reads") }
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.buttons["tasks.row.fixture-external-thread"]
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        try simulatorFixture("hold-history-reads")
        row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["task.loading"].waitForExistence(timeout: 2),
                      "Navigation must finish while the Host is still holding the history response")
        XCTAssertFalse(app.descendants(matching: .any)["task.empty"].exists)
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        XCTAssertFalse(app.buttons["task.attach"].isEnabled)
        try simulatorFixture("release-history-reads")
        let firstAnswer = app.descendants(matching: .any)["item.answer-fixture-external-thread"]
        XCTAssertTrue(firstAnswer.waitForExistence(timeout: 15))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("Keep this opening draft")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        try simulatorFixture("background-reply")
        try simulatorFixture("hold-history-reads")
        row.tap()
        XCTAssertTrue(firstAnswer.waitForExistence(timeout: 2),
                      "Restored history must display before its remote refresh completes")
        XCTAssertEqual(composer.value as? String, "Keep this opening draft")
        let latest = app.descendants(matching: .any)["item.fixture-external-final"]
        XCTAssertFalse(latest.exists)
        captureScreen(app, named: "Cached task opens while history refresh is pending")
        try simulatorFixture("release-history-reads")
        XCTAssertTrue(latest.waitForExistence(timeout: 15))
        XCTAssertEqual(composer.value as? String, "Keep this opening draft")
        XCTAssertFalse(app.staticTexts["notice"].exists)
    }

    func testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft() throws {
        let app = try connectedSimulatorApp()
        let response = try simulatorFixture("background-task", expectedStatus: 200, timeout: 15)
        let created = try JSONSerialization.jsonObject(with: response) as? [String: String]
        let id = try XCTUnwrap(created?["threadId"])
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.buttons["tasks.row.\(id)"]
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        try simulatorFixture("background-reply")
        try simulatorFixture("fail-next-history-read")
        row.tap()
        let retry = app.buttons["task.retry"]
        XCTAssertTrue(retry.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["notice"].exists)
        XCTAssertFalse(app.descendants(matching: .any)["task.loading"].exists)
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("Keep this failed-open draft")
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        retry.tap()
        let answer = app.descendants(matching: .any)["item.fixture-external-final"]
        XCTAssertTrue(answer.waitForExistence(timeout: 15))
        XCTAssertEqual(composer.value as? String, "Keep this failed-open draft")
        XCTAssertFalse(retry.exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        XCTAssertTrue(answer.waitForExistence(timeout: 10))
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
        XCTAssertTrue(firstAnswer.waitForExistence(timeout: 25))
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
        XCTAssertTrue(app.descendants(matching: .any)[secondAnswerID].waitForExistence(timeout: 25))
        XCTAssertTrue(app.staticTexts["Keep this retry draft"].exists)
        XCTAssertFalse(banner.exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        XCTAssertNotEqual(composer.value as? String, "Keep this retry draft")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let number = firstAnswerID.replacingOccurrences(of: "item.fixture-final-", with: "")
            .split(separator: "-")[0]
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[secondAnswerID].waitForExistence(timeout: 20))
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
        let row = app.descendants(matching: .any)["tasks.row.fixture-external-thread"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(
            app.descendants(matching: .any)["item.answer-fixture-external-thread"].waitForExistence(timeout: 15),
            "The external paginated conversation must open without taking its writer lock"
        )
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10)); composer.tap()
        composer.typeText("Keep this unsent draft")
        try simulatorFixture("background-reply")
        XCTAssertTrue(app.descendants(matching: .any)["item.fixture-external-final"].waitForExistence(timeout: 20),
                      "The open conversation did not update after another process persisted its reply")
        XCTAssertEqual(composer.value as? String, "Keep this unsent draft")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "External conversation updates with draft retained")
    }

    func testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Open before background update")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        let detail = app.descendants(matching: .any)["task.detail"]
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap(); composer.typeText("Keep this foreground draft")
        XCUIDevice.shared.press(.home)
        try simulatorFixture("background-reply")
        app.activate()
        XCTAssertTrue(detail.waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["item.fixture-external-final"].waitForExistence(timeout: 20),
                      "Foreground return did not fetch the conversation changed by another client")
        XCTAssertEqual(composer.value as? String, "Keep this foreground draft")
        captureScreen(app, named: "Latest conversation fetched on foreground")
    }

    func testSimulatorFetchesNewTaskAfterForeground() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        XCUIDevice.shared.press(.home)
        let response = try simulatorFixture("background-task", expectedStatus: 200, timeout: 15)
        let created = try JSONSerialization.jsonObject(with: response) as? [String: String]
        let identifier = try XCTUnwrap(created?["threadId"])
        app.activate()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.\(identifier)"].waitForExistence(timeout: 20),
                      "Foreground return did not fetch the conversation created by another client")
        captureScreen(app, named: "New conversation fetched on foreground")
    }

    func testSimulatorShowsWorkspaceConversationInsideItsProject() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let response = try simulatorFixture("background-task", expectedStatus: 200, timeout: 15)
        let created = try JSONSerialization.jsonObject(with: response) as? [String: String]
        let identifier = try XCTUnwrap(created?["threadId"])
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.\(identifier)"].waitForExistence(timeout: 20),
                      "The project conversation was absent from the refreshed list")
        let project = prefixedButton(app, prefix: "tasks.project.")
        XCTAssertTrue(project.waitForExistence(timeout: 10)); project.tap()
        let row = app.descendants(matching: .any)["tasks.row.\(identifier)"]
        let hidden = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: row)
        wait(for: [hidden], timeout: 10)
        project.tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        captureScreen(app, named: "Workspace conversation displayed inside its project")
    }

    func testSimulatorFetchesNewTaskWhenReturningToList() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Keep another conversation open")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        let response = try simulatorFixture("background-task", expectedStatus: 200, timeout: 15)
        let created = try JSONSerialization.jsonObject(with: response) as? [String: String]
        let identifier = try XCTUnwrap(created?["threadId"])
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.\(identifier)"].waitForExistence(timeout: 20),
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
        let row = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        XCTAssertEqual(row.value as? String, "実行中")
        captureScreen(app, named: "Task running in list")
        let completed = expectation(for: NSPredicate(format: "value == %@", "完了・未確認"), evaluatedWith: row)
        wait(for: [completed], timeout: 25)
        captureScreen(app, named: "White dot for unseen completion")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        XCTAssertTrue(row.waitForExistence(timeout: 20))
        XCTAssertEqual(row.value as? String, "完了・未確認")
        row.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        XCTAssertEqual(row.value as? String, "")
        captureScreen(app, named: "Completion dot cleared after opening")
    }
}
