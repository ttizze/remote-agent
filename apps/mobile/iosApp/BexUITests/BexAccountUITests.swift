import Foundation
import XCTest

extension BexLaunchUITests {
    func testSimulatorSwitchesCodexAccountsAndForksConversation() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        selectFixtureProvider(app, "Codex")
        openModelChoices(app)
        app.buttons["model.choice.fixture-model"].tap()
        app.buttons["model.close"].tap()
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("[success] Inherit this question")
        app.buttons["task.send"].tap()
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
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        XCTAssertTrue(app.buttons["model.choice.menu"].waitForExistence(timeout: 15))
        captureScreen(app, named: "Claude account and model on one screen")
        selectFixtureProvider(app, "Codex")
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 15))
        app.buttons["model.close"].tap()
    }

    func testSimulatorSignsInDirectlyFromModelSettings() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        let id = addFixtureClaudeAccount(app)
        XCTAssertTrue(app.buttons["model.choice.menu"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.navigationBars["モデルとアカウント"].exists)
        let logout = app.buttons["account.logout." + id]
        XCTAssertTrue(logout.isHittable); logout.tap()
        let confirm = app.alerts.buttons.matching(identifier: "account.logout.confirm").firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        app.alerts.buttons.matching(identifier: "account.logout.cancel").firstMatch.tap()
        XCTAssertTrue(logout.exists)
        logout.tap(); confirm.tap()
        let removed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: logout)
        wait(for: [removed], timeout: 15)
        captureScreen(app, named: "Sign out returns to the same settings screen")
    }

    func selectFixtureProvider(_ app: XCUIApplication, _ name: String) {
        let picker = app.segmentedControls["model.provider"]
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        if !picker.buttons[name].isSelected {
            picker.buttons[name].tap()
        }
    }

    func addFixtureClaudeAccount(_ app: XCUIApplication) -> String {
        selectFixtureProvider(app, "Claude")
        let existingIDs = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'account.logout.claude:'"))
            .allElementsBoundByIndex.map(\.identifier)
        let add = app.buttons["model.account.add"]
        XCTAssertTrue(add.waitForExistence(timeout: 10)); XCTAssertTrue(add.isHittable); add.tap()
        let code = app.secureTextFields["model.login.input"]
        XCTAssertTrue(code.waitForExistence(timeout: 15))
        XCTAssertTrue(app.navigationBars["モデルとアカウント"].exists)
        captureScreen(app, named: "Claude sign in stays on the same screen")
        code.tap(); code.typeText("fixture-code")
        app.buttons["model.login.submit"].tap()
        let logout = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH 'account.logout.claude:' AND NOT (identifier IN %@)", existingIDs
        )).firstMatch
        XCTAssertTrue(logout.waitForExistence(timeout: 20))
        return String(logout.identifier.dropFirst("account.logout.".count))
    }

    func testSimulatorOpensAccountManagementFromSettingsAndModelPicker() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.settings"].tap()
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["残り 72%"].exists)
        XCTAssertTrue(app.buttons["model.account.add"].isHittable)
        XCTAssertFalse(app.buttons["model.accounts.manage"].exists)
        captureScreen(app, named: "Unified settings from task list")
        app.buttons["model.close"].tap()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["残り 72%"].exists)
        XCTAssertTrue(app.buttons["model.account.add"].isHittable)
        XCTAssertTrue(app.buttons["model.choice.menu"].isHittable)
        XCTAssertFalse(app.buttons["model.accounts.manage"].exists)
        captureScreen(app, named: "Unified model and account settings")
    }

    func testSimulatorAccountOwnsModelEffortAndSpeed() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        openModelChoices(app)
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertTrue(app.buttons["model.choice.claude:default"].exists)
        app.buttons["model.choice.claude:default"].tap()
        let effort = fixtureEffortControl(app)
        XCTAssertTrue(effort.buttons["high"].isSelected)
        XCTAssertTrue(effort.buttons["low"].exists)
        XCTAssertFalse(effort.buttons["medium"].exists)
        XCTAssertFalse(app.pickers["model.service-tier"].exists)
        captureScreen(app, named: "Claude owns its model and effort in unified settings")
        openModelChoices(app)
        app.buttons["model.choice.claude:haiku"].tap()
        XCTAssertFalse(app.segmentedControls["model.quick.effort"].exists)
        selectFixtureProvider(app, "Codex")
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 15))
        openModelChoices(app)
        XCTAssertFalse(app.buttons["model.choice.claude:default"].exists)
        app.buttons["model.choice.fixture-model"].tap()
        XCTAssertTrue(fixtureEffortControl(app).buttons["medium"].isSelected)
        captureScreen(app, named: "Codex owns its model and effort in unified settings")
    }

    private func switchFixtureAccount(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 10))
        app.buttons["model.account.add"].tap()
        XCTAssertTrue(app.staticTexts["model.login.code"].waitForExistence(timeout: 10))
        XCUIDevice.shared.press(.home)
        app.activate()
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'account.logout.account-'"))
            .firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 20))
        let id = String(second.identifier.dropFirst("account.logout.".count))
        app.buttons["model.account.picker"].tap()
        XCTAssertTrue(app.buttons["model.account.desktop"].waitForExistence(timeout: 5))
        app.buttons["model.account." + id].tap()
        openModelChoices(app)
        app.buttons["model.choice.fixture-model"].tap()
        fixtureEffortControl(app).buttons["high"].tap()
        captureScreen(app, named: "Account dropdown switches accounts without leaving settings")
        app.buttons["model.close"].tap()
    }
}
