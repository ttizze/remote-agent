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
        openAccountManagement(app)
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
                format: "identifier BEGINSWITH 'model.account.' AND identifier != 'model.account.add'"
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
                    format: "identifier BEGINSWITH 'model.account.' AND identifier != 'model.account.add'"
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
        XCTAssertTrue(app.navigationBars["エージェント"].exists)
        let actions = app.buttons["account.actions." + id]
        for _ in 0 ..< 6 where !actions.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(actions.isHittable); actions.tap()
        app.buttons["account.logout." + id].tap()
        let confirm = app.alerts.buttons.matching(identifier: "account.logout.confirm").firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        app.alerts.buttons.matching(identifier: "account.logout.cancel").firstMatch.tap()
        XCTAssertTrue(actions.exists)
        actions.tap()
        app.buttons["account.logout." + id].tap(); confirm.tap()
        let removed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: actions)
        wait(for: [removed], timeout: 15)
        captureScreen(app, named: "Account management preserves sign-out confirmation")
    }

    func selectFixtureProvider(_ app: XCUIApplication, _ name: String) {
        openModelChoices(app)
        let picker = app.buttons["model.provider"]
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        picker.tap(); app.buttons[name == "Codex" ? "Codex" : "Claude Code"].tap()
    }

    func openAccountManagement(_ app: XCUIApplication) {
        let manage = app.buttons["model.accounts.manage"]
        XCTAssertTrue(manage.waitForExistence(timeout: 10)); manage.tap()
    }

    func addFixtureClaudeAccount(_ app: XCUIApplication) -> String {
        selectFixtureProvider(app, "Claude")
        openAccountManagement(app)
        let existingIDs = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.claude:'"))
            .allElementsBoundByIndex.map(\.identifier)
        let add = app.buttons["model.account.add"]
        XCTAssertTrue(add.waitForExistence(timeout: 10)); XCTAssertTrue(add.isHittable); add.tap()
        let code = app.secureTextFields["model.login.input"]
        XCTAssertTrue(code.waitForExistence(timeout: 15))
        XCTAssertTrue(app.navigationBars["エージェント"].exists)
        captureScreen(app, named: "Claude sign in from account management")
        code.tap(); code.typeText("fixture-code")
        app.buttons["model.login.submit"].tap()
        let logout = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH 'model.account.claude:' AND NOT (identifier IN %@)", existingIDs
        )).firstMatch
        XCTAssertTrue(logout.waitForExistence(timeout: 20))
        return String(logout.identifier.dropFirst("model.account.".count))
    }

    func testSimulatorOpensAccountManagementFromSettingsAndModelPicker() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        XCTAssertTrue(app.buttons["settings.agents"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["settings.connections"].exists)
        XCTAssertTrue(app.buttons["settings.worktrees"].exists)
        XCTAssertFalse(app.segmentedControls["account.provider"].exists)
        let environment = app.buttons["settings.scope.environment"]
        XCTAssertTrue(environment.label.contains("検証 Host"))
        captureScreen(app, named: "Settings navigation list")
        app.buttons["settings.agents"].tap()
        XCTAssertTrue(app.buttons["account.actions.desktop"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["すべてのプロジェクト"].exists)
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "週間残量 86%"))
            .firstMatch.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["model.account.add"].isHittable)
        app.segmentedControls["account.provider"].buttons["Claude Code"].tap()
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        app.segmentedControls["account.provider"].buttons["Codex"].tap()
        captureScreen(app, named: "Agent account settings")
        app.buttons["model.back"].tap()
        app.buttons["settings.connections"].tap()
        XCTAssertTrue(app.buttons["profiles.add"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["このiPhone"].exists)
        app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'profiles.' AND identifier != 'profiles.add'"))
            .firstMatch.tap()
        XCTAssertTrue(app.buttons["tasks.new.project.simulator-project"].waitForExistence(timeout: 10))
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        app.buttons["model.settings"].tap()
        XCTAssertTrue(app.buttons["model.provider"].isEnabled)
        XCTAssertFalse(app.buttons["settings.scope.projects"].exists || app.buttons["settings.scope.environment"]
            .exists)
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        XCTAssertFalse(app.buttons["account.actions.desktop"].exists)
        captureScreen(app, named: "Model picker with read-only account")
        openAccountManagement(app)
        app.segmentedControls["account.provider"].buttons["Claude Code"].tap()
        openModelChoices(app)
        XCTAssertEqual(app.buttons["model.choice.fixture-model"].value as? String, "選択中",
                       "Browsing agent settings must not change the draft model")
        app.buttons["model.close"].tap()
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("[success] Keep the conversation agent")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        app.buttons["model.settings"].tap()
        XCTAssertFalse(app.buttons["model.provider"].isEnabled)
        XCTAssertTrue(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertFalse(app.buttons["model.choice.default"].exists)
        captureScreen(app, named: "Existing conversation fixes the agent")
    }

    func testSimulatorAutomaticallyShowsModelControlsInExistingAndRunningConversations() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("external-conversation")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.buttons["tasks.row.codex:fixture-external-thread"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-fixture-external-thread"]
            .waitForExistence(timeout: 15))
        captureScreen(app, named: "Model controls on an externally created conversation")
        let model = app.buttons["model.settings"]
        XCTAssertEqual(model.value as? String, "Fixture Model",
                       "Opening an existing conversation must select from the automatically loaded model catalog")
        XCTAssertTrue(app.buttons["model.effort"].isHittable)
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("[delayed-input] Keep model settings available while working")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedButton(app, prefix: "turn.interrupt.").waitForExistence(timeout: 10))
        XCTAssertTrue(model.isHittable)
        XCTAssertEqual(model.value as? String, "Fixture Model")
        captureScreen(app, named: "Model selection remains visible during a running turn")
        model.tap()
        let selected = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(selected.waitForExistence(timeout: 10))
        XCTAssertEqual(selected.value as? String, "選択中")
        app.buttons["model.close"].tap()
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
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

    func testSimulatorModelDefaultsPersistAndApplyOnlyToNewConversations() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        chooseFixtureEffort(app, "medium")
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("Keep this existing draft")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.settings"].tap()
        let models = app.buttons["settings.models"]
        XCTAssertTrue(models.waitForExistence(timeout: 10)); models.tap()
        XCTAssertTrue(app.buttons["model.choice.automatic"].waitForExistence(timeout: 10))
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15)); choice.tap()
        XCTAssertFalse(app.descendants(matching: .any)["model.error"].exists)
        let effort = app.buttons["model.sheet.effort"]
        XCTAssertTrue(effort.waitForExistence(timeout: 10)); effort.tap()
        app.buttons["high"].tap()
        let speed = app.buttons["model.defaults.speed"]
        XCTAssertTrue(speed.exists); speed.tap()
        app.buttons["高速"].tap()
        captureScreen(app, named: "Default model effort and speed for new conversations")
        app.buttons["model.close"].tap()
        app.buttons["tasks.new.project.simulator-project"].tap()
        XCTAssertEqual(app.textFields["task.message"].value as? String, "Keep this existing draft")
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "medium")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オフ")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["tasks.new.chat"].tap()
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オン")
        app.buttons["model.settings"].tap()
        XCTAssertEqual(app.buttons["model.choice.fixture-model"].value as? String, "選択中")
        XCTAssertFalse(app.descendants(matching: .any)["model.error"].exists)
        app.buttons["model.close"].tap()
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.menu"].waitForExistence(timeout: 20))
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        XCTAssertEqual(app.buttons["model.choice.fixture-model"].value as? String, "選択中")
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.defaults.speed"].value as? String, "高速")
        app.buttons["model.choice.automatic"].tap()
        XCTAssertEqual(app.buttons["model.choice.automatic"].value as? String, "選択中")
    }

    private func switchFixtureAccount(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        openAccountManagement(app)
        XCTAssertTrue(app.buttons["account.actions.desktop"].waitForExistence(timeout: 10))
        app.buttons["model.account.add"].tap()
        XCTAssertTrue(app.staticTexts["model.login.code"].waitForExistence(timeout: 10))
        XCUIDevice.shared.press(.home)
        app.activate()
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.account-'"))
            .firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 20))
        let id = String(second.identifier.dropFirst("model.account.".count))
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
