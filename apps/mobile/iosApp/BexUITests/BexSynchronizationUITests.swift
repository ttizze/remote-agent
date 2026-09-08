import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
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
