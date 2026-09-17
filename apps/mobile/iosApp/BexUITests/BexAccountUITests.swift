import Foundation
import XCTest

extension BexLaunchUITests {
    func testSimulatorSwitchesCodexAccountsAndForksConversation() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Inherit this question")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let originalAnswerID = answer.identifier
        let firstFork = app.buttons["response.fork." + String(originalAnswerID.dropFirst("item.".count))]
        XCTAssertTrue(firstFork.waitForExistence(timeout: 10))
        switchFixtureAccount(app)
        XCTAssertTrue(app.descendants(matching: .any)[originalAnswerID].exists)
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("[success] This later question stays in the original")
        app.buttons["task.send"].tap()
        let nextID = String(originalAnswerID.dropLast()) + "2"
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitForExistence(timeout: 25))
        // Fork the earlier completed turn after the original has a later turn.
        let list = app.descendants(matching: .any)["task.detail"]
        for _ in 0 ..< 6 where !firstFork.isHittable {
            list.swipeDown()
        }
        XCTAssertTrue(firstFork.isHittable); firstFork.tap()
        let disappeared = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: app.descendants(matching: .any)[nextID]
        )
        wait(for: [disappeared], timeout: 20)
        XCTAssertTrue(app.descendants(matching: .any)[originalAnswerID].waitForExistence(timeout: 20))
        composer.tap(); composer.typeText("[success] Continue the fork")
        app.buttons["task.send"].tap()
        let forkAnswer = app.descendants(matching: .any).matching(NSPredicate(
            format: "identifier BEGINSWITH 'item.fixture-final-' AND identifier != %@",
            originalAnswerID
        )).firstMatch
        XCTAssertTrue(forkAnswer.waitForExistence(timeout: 25))
        XCTAssertNotEqual(forkAnswer.identifier, nextID)
        captureScreen(app, named: "Fork inherits the selected answer and accepts a new turn")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let number = try XCTUnwrap(originalAnswerID.split(separator: "-").dropLast().last)
        let original = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(original.waitForExistence(timeout: 15)); original.tap()
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitForExistence(timeout: 20))
    }

    func testSimulatorAddsClaudeAccountAndKeepsCodexSelected() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        app.buttons["model.settings"].tap()
        let desktop = app.buttons["model.account.desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 10))
        openAccountManagement(app)
        _ = addFixtureClaudeAccount(app)
        XCTAssertEqual(desktop.value as? String, "選択中")
        captureScreen(app, named: "Claude account added alongside Codex")
        app.navigationBars["アカウント"].buttons.element(boundBy: 0).tap()
        app.buttons["model.close"].tap()
    }

    func addFixtureClaudeAccount(_ app: XCUIApplication) -> String {
        let existingIDs = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.claude:'"))
            .allElementsBoundByIndex.map(\.identifier)
        let add = app.buttons["model.account.add.claude"]
        for _ in 0 ..< 5 where !add.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(add.isHittable); add.tap()
        let code = app.secureTextFields["model.login.input"]
        XCTAssertTrue(code.waitForExistence(timeout: 15))
        code.tap(); code.typeText("fixture-code")
        app.buttons["model.login.submit"].tap()
        let account = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH 'model.account.claude:' AND NOT (identifier IN %@)", existingIDs
        )).firstMatch
        XCTAssertTrue(account.waitForExistence(timeout: 20))
        let selected = expectation(for: NSPredicate(format: "value == %@", "選択中"), evaluatedWith: account)
        wait(for: [selected], timeout: 15)
        return account.identifier
    }

    func openAccountManagement(_ app: XCUIApplication) {
        let manage = app.buttons["model.accounts.manage"]
        for _ in 0 ..< 6 where !manage.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(manage.isHittable); manage.tap()
        XCTAssertTrue(app.navigationBars["アカウント"].waitForExistence(timeout: 10))
    }

    func testSimulatorOpensAccountManagementFromSettingsAndModelPicker() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.settings"].tap()
        app.buttons["settings.accounts"].tap()
        let desktop = app.buttons["model.account.desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["残り 72%"].exists)
        XCTAssertTrue(app.buttons["account.logout.desktop"].exists)
        captureScreen(app, named: "Account usage from settings")
        app.navigationBars["アカウント"].buttons.element(boundBy: 0).tap()
        app.buttons["settings.close"].tap()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        XCTAssertTrue(desktop.waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["残り 72%"].exists)
        captureScreen(app, named: "Account usage in model picker")
        openAccountManagement(app)
        XCTAssertTrue(app.buttons["account.logout.desktop"].exists)
    }

    func testSimulatorAccountOwnsModelEffortAndSpeed() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        openAccountManagement(app)
        let claudeID = addFixtureClaudeAccount(app)
        app.navigationBars["アカウント"].buttons.element(boundBy: 0).tap()
        openModelChoices(app)
        XCTAssertFalse(app.buttons["model.choice.claude:default"].exists)
        app.buttons["model.choice.fixture-model"].tap()
        let claude = app.buttons[claudeID]
        for _ in 0 ..< 6 where !claude.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(claude.isHittable); claude.tap()
        let selected = expectation(for: NSPredicate(format: "value == %@", "選択中"), evaluatedWith: claude)
        wait(for: [selected], timeout: 20)
        openModelChoices(app)
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertTrue(app.buttons["model.choice.claude:default"].exists)
        app.buttons["model.choice.claude:default"].tap()
        let effort = fixtureEffortControl(app)
        XCTAssertTrue(effort.buttons["high"].isSelected)
        XCTAssertTrue(effort.buttons["low"].exists)
        XCTAssertFalse(effort.buttons["medium"].exists)
        XCTAssertFalse(app.pickers["model.service-tier"].exists)
        captureScreen(app, named: "Claude account owns its model and effort")
        openModelChoices(app)
        app.buttons["model.choice.claude:haiku"].tap()
        XCTAssertFalse(app.segmentedControls["model.quick.effort"].exists)
        let codex = app.buttons["model.account.desktop"]
        for _ in 0 ..< 6 where !codex.isHittable {
            app.swipeDown()
        }
        XCTAssertTrue(codex.isHittable); codex.tap()
        let restored = expectation(for: NSPredicate(format: "value == %@", "選択中"), evaluatedWith: codex)
        wait(for: [restored], timeout: 20)
        XCTAssertTrue(fixtureEffortControl(app).buttons["medium"].isSelected)
        captureScreen(app, named: "Codex account owns its model and effort")
    }

    private func switchFixtureAccount(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        let desktop = app.buttons["model.account.desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 10))
        XCTAssertEqual(desktop.value as? String, "選択中")
        openAccountManagement(app)
        app.buttons["model.account.add"].tap()
        XCTAssertTrue(app.staticTexts["model.login.code"].waitForExistence(timeout: 10))
        XCUIDevice.shared.press(.home)
        app.activate()
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.account-'"))
            .firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 20))
        let selected = expectation(for: NSPredicate(format: "value == %@", "選択中"), evaluatedWith: second)
        wait(for: [selected], timeout: 15)
        app.navigationBars["アカウント"].buttons.element(boundBy: 0).tap()
        openModelChoices(app)
        app.buttons["model.choice.fixture-model"].tap()
        fixtureEffortControl(app).buttons["high"].tap()
        captureScreen(app, named: "Two Codex accounts sharing conversation history")
        app.buttons["model.close"].tap()
    }
}
