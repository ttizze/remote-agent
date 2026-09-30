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
        let original = app.descendants(matching: .any)["tasks.row.codex:fixture-thread-\(number)"]
        XCTAssertTrue(original.waitForExistence(timeout: 15)); original.tap()
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitForExistence(timeout: 20))
    }

    func testSimulatorAddsClaudeAccountAndKeepsCodexSelected() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        openModelChoices(app)
        XCTAssertTrue(app.buttons["model.choice.default"].waitForExistence(timeout: 15))
        selectFixtureProvider(app, "Codex")
        app.buttons["model.accounts"].tap()
        XCTAssertEqual(app.buttons["model.account.desktop"].value as? String, "選択中")
        app.buttons["model.close"].tap()
    }

    func testSimulatorGoesBackFromAccountLoginAndCanStartAgain() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        for provider in ["Codex", "Claude"] {
            selectFixtureProvider(app, provider)
            openAccountManagement(app)
            let originalAccounts = app.buttons.matching(NSPredicate(
                format: "identifier BEGINSWITH 'account.logout.'"
            )).allElementsBoundByIndex.map(\.identifier).sorted()
            for attempt in 0 ..< 2 {
                let add = app.buttons["model.account.add"]
                XCTAssertTrue(add.waitForExistence(timeout: 10)); add.tap()
                if attempt == 1 {
                    let code = provider == "Codex"
                        ? app.staticTexts["model.login.code"] : app.secureTextFields["model.login.input"]
                    XCTAssertTrue(code.waitForExistence(timeout: 15))
                    if provider == "Claude" {
                        code.tap(); code.typeText("unfinished-code")
                    }
                }
                let back = app.navigationBars.buttons["model.back"]
                XCTAssertTrue(back.waitForExistence(timeout: 10))
                XCTAssertTrue(back.isEnabled && back.isHittable)
                XCTAssertFalse(app.buttons["model.login.cancel"].exists)
                back.tap()
                let cancelled = expectation(
                    for: NSPredicate(format: "exists == true AND enabled == true"), evaluatedWith: add
                )
                wait(for: [cancelled], timeout: 15)
                XCTAssertEqual(app.buttons.matching(NSPredicate(
                    format: "identifier BEGINSWITH 'account.logout.'"
                )).allElementsBoundByIndex.map(\.identifier).sorted(), originalAccounts)
            }
        }
        app.buttons["model.close"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    func testSimulatorSignsInDirectlyFromModelSettings() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        let id = addFixtureClaudeAccount(app)
        XCTAssertTrue(app.navigationBars["アカウントを管理"].exists)
        let logout = app.buttons["account.logout." + id]
        for _ in 0 ..< 6 where !logout.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(logout.isHittable); logout.tap()
        let confirm = app.alerts.buttons.matching(identifier: "account.logout.confirm").firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        app.alerts.buttons.matching(identifier: "account.logout.cancel").firstMatch.tap()
        XCTAssertTrue(logout.exists)
        logout.tap(); confirm.tap()
        let removed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: logout)
        wait(for: [removed], timeout: 15)
        captureScreen(app, named: "Account management preserves sign-out confirmation")
    }

    func selectFixtureProvider(_ app: XCUIApplication, _ name: String) {
        openModelChoices(app)
        let picker = app.buttons["model.provider"]
        XCTAssertTrue(picker.waitForExistence(timeout: 10)); picker.tap()
        app.buttons["model.provider." + (name == "Codex" ? "codex" : "claude")].tap()
    }

    func openAccountManagement(_ app: XCUIApplication) {
        app.buttons["model.accounts"].tap()
        let manage = app.buttons["model.accounts.manage"]
        XCTAssertTrue(manage.waitForExistence(timeout: 10)); manage.tap()
    }

    func addFixtureClaudeAccount(_ app: XCUIApplication) -> String {
        selectFixtureProvider(app, "Claude")
        openAccountManagement(app)
        let existingIDs = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'account.logout.claude:'"))
            .allElementsBoundByIndex.map(\.identifier)
        let add = app.buttons["model.account.add"]
        XCTAssertTrue(add.waitForExistence(timeout: 10)); XCTAssertTrue(add.isHittable); add.tap()
        let code = app.secureTextFields["model.login.input"]
        XCTAssertTrue(code.waitForExistence(timeout: 15))
        XCTAssertTrue(app.navigationBars["アカウントを管理"].exists)
        captureScreen(app, named: "Claude sign in from account management")
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
        captureScreen(app, named: "Account management from settings")
        app.buttons["model.close"].tap()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        selectFixtureProvider(app, "Codex")
        XCTAssertTrue(app.buttons["model.accounts"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["account.logout.desktop"].exists)
        XCTAssertFalse(app.buttons["model.accounts.manage"].exists)
        captureScreen(app, named: "Model picker with weekly quota entry")
        app.buttons["model.accounts"].tap()
        XCTAssertTrue(app.buttons["model.account.desktop"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["account.logout.desktop"].exists)
        app.buttons["model.accounts.manage"].tap()
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["model.account.add"].isHittable)
        captureScreen(app, named: "Weekly quota opens account switching and management")
    }

    func testSimulatorComposerOffersFastModelAndEffortBeforeMicrophone() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        let fast = app.buttons["model.fast"]
        let model = app.buttons["model.settings"]
        let effort = app.buttons["model.effort"]
        let microphone = app.buttons["dictation.toggle"]
        let send = app.buttons["task.send"]
        XCTAssertTrue(fast.isHittable && effort.isHittable)
        XCTAssertLessThanOrEqual(fast.frame.maxX, model.frame.minX + 1)
        XCTAssertLessThanOrEqual(model.frame.maxX, effort.frame.minX + 1)
        XCTAssertLessThanOrEqual(effort.frame.maxX, microphone.frame.minX + 1)
        XCTAssertLessThanOrEqual(microphone.frame.maxX, send.frame.minX + 1)
        XCTAssertEqual(fast.value as? String, "オフ")
        fast.tap()
        XCTAssertEqual(fast.value as? String, "オン")
        fast.tap()
        XCTAssertEqual(fast.value as? String, "オフ")
        chooseFixtureEffort(app, "medium")
        XCTAssertEqual(effort.value as? String, "medium")
        captureScreen(app, named: "Borderless Fast model effort microphone send toolbar")
        model.tap()
        _ = addFixtureClaudeAccount(app)
        openModelChoices(app)
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        app.buttons["model.choice.default"].tap()
        app.buttons["model.close"].tap()
        XCTAssertFalse(fast.exists)
        XCTAssertEqual(effort.value as? String, "high")
        effort.tap()
        XCTAssertTrue(app.buttons["model.effort.low"].exists)
        XCTAssertFalse(app.buttons["model.effort.medium"].exists)
        app.buttons["model.effort.high"].tap()
        model.tap()
        app.buttons["model.choice.haiku"].tap()
        app.buttons["model.close"].tap()
        XCTAssertFalse(effort.exists)
        model.tap()
        selectFixtureProvider(app, "Codex")
        app.buttons["model.choice.fixture-model"].tap()
        let search = app.textFields["model.search"]
        search.tap(); search.typeText("no matching model")
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        app.buttons["model.close"].tap()
        XCTAssertTrue(fast.exists && effort.exists)
    }

    private func switchFixtureAccount(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        openAccountManagement(app)
        XCTAssertTrue(app.buttons["account.logout.desktop"].waitForExistence(timeout: 10))
        app.buttons["model.account.add"].tap()
        XCTAssertTrue(app.staticTexts["model.login.code"].waitForExistence(timeout: 10))
        XCUIDevice.shared.press(.home)
        app.activate()
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'account.logout.account-'"))
            .firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 20))
        let id = String(second.identifier.dropFirst("account.logout.".count))
        XCTAssertTrue(app.buttons["model.account.desktop"].waitForExistence(timeout: 5))
        app.buttons["model.account.desktop"].tap()
        let choice = app.buttons["model.account." + id]
        for _ in 0 ..< 6 where !choice.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(choice.isHittable)
        choice.tap()
        openModelChoices(app)
        app.buttons["model.choice.fixture-model"].tap()
        app.buttons["model.close"].tap()
        chooseFixtureEffort(app, "high")
        captureScreen(app, named: "Account switches preserve conversation and quick controls")
    }
}
