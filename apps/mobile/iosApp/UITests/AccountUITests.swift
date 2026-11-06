import Foundation
import XCTest

extension LaunchUITests {
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
        dismissModelSettings(app)
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("[success] Inherit this question")
        app.buttons["task.send"].tap()
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitUntilExists(timeout: 25))
        let originalAnswerID = answer.identifier
        let firstFork = app.buttons["response.fork." + String(originalAnswerID.dropFirst("item.".count))]
        XCTAssertTrue(firstFork.waitUntilExists(timeout: 10))
        switchFixtureAccount(app)
        XCTAssertTrue(app.descendants(matching: .any)[originalAnswerID].exists)
        composer.tap(); composer.typeText("[success] This later question stays in the original")
        app.buttons["task.send"].tap()
        let nextID = String(originalAnswerID.dropLast()) + "2"
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitUntilExists(timeout: 25))
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
        XCTAssertTrue(app.descendants(matching: .any)[originalAnswerID].waitUntilExists(timeout: 20))
        composer.tap(); composer.typeText("[success] Continue the fork")
        app.buttons["task.send"].tap()
        let forkAnswer = app.descendants(matching: .any).matching(NSPredicate(
            format: "identifier BEGINSWITH 'item.fixture-final-' AND identifier != %@",
            originalAnswerID
        )).firstMatch
        XCTAssertTrue(forkAnswer.waitUntilExists(timeout: 25))
        XCTAssertNotEqual(forkAnswer.identifier, nextID)
        captureScreen(app, named: "Fork inherits the selected answer and accepts a new turn")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let number = try XCTUnwrap(originalAnswerID.split(separator: "-").dropLast().last)
        let original = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(number)")]
        XCTAssertTrue(original.waitUntilExists(timeout: 15)); original.tap()
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitUntilExists(timeout: 20))
    }

    func testSimulatorAddsClaudeAccountAndKeepsCodexSelected() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        XCTAssertTrue(app.staticTexts["5時間枠"].waitUntilExists(timeout: 15))
        XCTAssertTrue(app.staticTexts["Fable · 週間枠"].waitUntilExists(timeout: 15))
        XCTAssertTrue(app.staticTexts["残り 66%"].exists)
        openModelChoices(app)
        XCTAssertTrue(app.buttons["model.choice.default"].waitUntilExists(timeout: 15))
        selectFixtureProvider(app, "Codex")
        openAccountManagement(app)
        XCTAssertEqual(app.buttons["model.account.desktop"].value as? String, "選択中")
        dismissModelSettings(app)
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
                XCTAssertTrue(add.waitUntilExists(timeout: 10)); add.tap()
                if attempt == 1 {
                    let code = provider == "Codex"
                        ? app.staticTexts["model.login.code"] : app.secureTextFields["model.login.input"]
                    XCTAssertTrue(code.waitUntilExists(timeout: 15))
                    if provider == "Claude" {
                        code.tap(); code.typeText("unfinished-code")
                    }
                }
                let back = app.navigationBars.buttons["model.back"]
                XCTAssertTrue(back.waitUntilExists(timeout: 10))
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
        dismissModelSettings(app)
        XCTAssertTrue(app.textFields["task.message"].waitUntilExists(timeout: 10))
    }

    func testSimulatorSignsInDirectlyFromModelSettings() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        let id = addFixtureClaudeAccount(app)
        XCTAssertTrue(app.navigationBars["Claude Code"].exists)
        let actions = app.buttons["account.actions." + id]
        for _ in 0 ..< 6 where !actions.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(actions.isHittable); actions.tap()
        app.buttons["account.logout." + id].tap()
        let confirm = app.alerts.buttons.matching(identifier: "account.logout.confirm").firstMatch
        XCTAssertTrue(confirm.waitUntilExists(timeout: 5))
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
        let provider = app.segmentedControls["model.provider"].buttons[name == "Codex" ? "Codex" : "Claude Code"]
        scrollToEarlierListElement(provider, in: app, attempts: 6)
        provider.tap()
    }

    func openAccountManagement(_ app: XCUIApplication) {
        let manage = app.buttons["model.accounts.manage"]
        scrollToListElement(manage, in: app, bottomMargin: 0)
        manage.tap()
    }

    func assertAccountQuota(_ app: XCUIApplication, label: String, remaining: Int) {
        XCTAssertTrue(app.staticTexts[label].waitUntilExists(timeout: 15))
        XCTAssertTrue(app.staticTexts[label].isHittable)
        XCTAssertTrue(app.staticTexts["残り \(remaining)%"].exists)
    }

    func addFixtureClaudeAccount(_ app: XCUIApplication) -> String {
        selectFixtureProvider(app, "Claude")
        openAccountManagement(app)
        let existingIDs = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.claude:'"))
            .allElementsBoundByIndex.map(\.identifier)
        let add = app.buttons["model.account.add"]
        XCTAssertTrue(add.waitUntilExists(timeout: 10)); XCTAssertTrue(add.isHittable); add.tap()
        let code = app.secureTextFields["model.login.input"]
        XCTAssertTrue(code.waitUntilExists(timeout: 15))
        XCTAssertTrue(app.navigationBars["Claude Code"].exists)
        captureScreen(app, named: "Claude sign in from account management")
        code.tap(); code.typeText("fixture-code")
        app.buttons["model.login.submit"].tap()
        let logout = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH 'model.account.claude:' AND NOT (identifier IN %@)", existingIDs
        )).firstMatch
        XCTAssertTrue(logout.waitUntilExists(timeout: 20))
        return String(logout.identifier.dropFirst("model.account.".count))
    }

    func testSimulatorOpensAccountManagementFromSettingsAndModelPicker() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        XCTAssertTrue(app.buttons["settings.providers"].waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["settings.connections"].exists)
        XCTAssertTrue(app.buttons["settings.worktrees"].exists)
        XCTAssertFalse(app.segmentedControls["account.provider"].exists)
        XCTAssertFalse(app.sheets.firstMatch.exists)
        captureScreen(app, named: "Settings navigation list")
        app.buttons["settings.providers"].tap()
        let environment = app.buttons["settings.scope.environment"]
        XCTAssertTrue(environment.label.contains("検証 Host"))
        XCTAssertTrue(app.buttons["provider.codex"].waitUntilExists(timeout: 10))
        XCTAssertTrue(app.switches["provider.enabled.codex"].exists)
        app.buttons["provider.codex"].tap()
        XCTAssertTrue(app.buttons["account.actions.desktop"].waitUntilExists(timeout: 15))
        let scope = app.descendants(matching: .any).matching(NSPredicate(
            format: "label CONTAINS %@ OR value == %@", "すべてのプロジェクト", "すべてのプロジェクト"
        )).firstMatch
        XCTAssertTrue(scope.exists && scope.isHittable)
        assertAccountQuota(app, label: "5時間枠", remaining: 72)
        assertAccountQuota(app, label: "週間枠", remaining: 86)
        XCTAssertTrue(app.buttons["model.account.add"].isHittable)
        app.navigationBars["Codex"].buttons["BackButton"].tap()
        app.buttons["provider.claude"].tap()
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        app.navigationBars["Claude Code"].buttons["BackButton"].tap()
        app.buttons["provider.codex"].tap()
        captureScreen(app, named: "Provider account settings")
        app.navigationBars["Codex"].buttons["BackButton"].tap()
        app.navigationBars["プロバイダー"].buttons["BackButton"].tap()
        app.buttons["settings.connections"].tap()
        XCTAssertTrue(app.buttons["profiles.add"].waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["pairing.scan"].exists)
        app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'profiles.' AND identifier != 'profiles.add'"))
            .firstMatch.tap()
        XCTAssertTrue(app.switches["environment.enabled"].waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["providers.refresh"].exists)
        app.navigationBars["検証 Host"].buttons["BackButton"].tap()
        app.navigationBars["環境"].buttons["BackButton"].tap()
        app.buttons["settings.close"].tap()
        XCTAssertTrue(app.buttons["tasks.new.project.simulator-project"].waitUntilExists(timeout: 10))
        verifyConversationAccountControls(app)
    }

    private func verifyConversationAccountControls(_ app: XCUIApplication) {
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        app.buttons["model.settings"].tap()
        XCTAssertTrue(app.segmentedControls["model.provider"].buttons["Codex"].isEnabled)
        XCTAssertFalse(app.buttons["settings.scope.projects"].exists || app.buttons["settings.scope.environment"]
            .exists)
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        XCTAssertFalse(app.buttons["account.actions.desktop"].exists)
        captureScreen(app, named: "Model picker with read-only account")
        openAccountManagement(app)
        XCTAssertTrue(app.navigationBars["Codex"].exists)
        XCTAssertTrue(app.buttons["account.refresh"].exists)
        openModelChoices(app)
        XCTAssertTrue(app.buttons["model.choice.fixture-model"].isSelected,
                      "Browsing agent settings must not change the draft model")
        dismissModelSettings(app)
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("[success] Keep the conversation agent")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 25))
        expandComposer(app)
        app.buttons["model.settings"].tap()
        XCTAssertTrue(app.segmentedControls["model.provider"].buttons["Codex"].isSelected)
        XCTAssertTrue(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertFalse(app.buttons["model.choice.default"].exists)
        captureScreen(app, named: "Existing conversation retains the selected agent")
    }

    func testSimulatorAutomaticallyShowsModelControlsInExistingAndRunningConversations() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("external-conversation")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.buttons["tasks.row.codex:fixture-external-thread"]
        XCTAssertTrue(row.waitUntilExists(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-fixture-external-thread"]
            .waitUntilExists(timeout: 15))
        expandComposer(app)
        captureScreen(app, named: "Model controls on an externally created conversation")
        let model = app.buttons["model.settings"]
        XCTAssertEqual(model.value as? String, "Fixture Model",
                       "Opening an existing conversation must select from the automatically loaded model catalog")
        XCTAssertTrue(app.buttons["model.effort"].isHittable)
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("[delayed-input] Keep model settings available while working")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedButton(app, prefix: "turn.interrupt.").waitUntilExists(timeout: 10))
        expandComposer(app)
        XCTAssertTrue(model.isHittable)
        XCTAssertEqual(model.value as? String, "Fixture Model")
        captureScreen(app, named: "Model selection remains visible during a running turn")
        model.tap()
        let selected = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(selected.waitUntilExists(timeout: 10))
        XCTAssertTrue(selected.isSelected)
        dismissModelSettings(app)
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 25))
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
        dismissModelSettings(app)
        XCTAssertFalse(fast.exists)
        XCTAssertEqual(effort.value as? String, "high")
        effort.tap()
        XCTAssertTrue(app.buttons["model.effort.low"].exists)
        XCTAssertFalse(app.buttons["model.effort.medium"].exists)
        app.buttons["model.effort.high"].tap()
        model.tap()
        app.buttons["model.choice.haiku"].tap()
        dismissModelSettings(app)
        XCTAssertFalse(effort.exists)
        model.tap()
        selectFixtureProvider(app, "Codex")
        app.buttons["model.choice.fixture-model"].tap()
        let search = app.searchFields.firstMatch
        search.tap(); search.typeText("no matching model")
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        dismissModelSettings(app)
        XCTAssertTrue(fast.exists && effort.exists)
    }

    private func switchFixtureAccount(_ app: XCUIApplication) {
        expandComposer(app)
        app.buttons["model.settings"].tap()
        openAccountManagement(app)
        XCTAssertTrue(app.buttons["account.actions.desktop"].waitUntilExists(timeout: 10))
        app.buttons["model.account.add"].tap()
        XCTAssertTrue(app.staticTexts["model.login.code"].waitUntilExists(timeout: 10))
        XCUIDevice.shared.press(.home)
        app.activate()
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.account-'"))
            .firstMatch
        XCTAssertTrue(second.waitUntilExists(timeout: 20))
        let id = String(second.identifier.dropFirst("model.account.".count))
        XCTAssertTrue(app.buttons["model.account.desktop"].waitUntilExists(timeout: 5))
        app.buttons["model.account.desktop"].tap()
        let choice = app.buttons["model.account." + id]
        for _ in 0 ..< 6 where !choice.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(choice.isHittable)
        choice.tap()
        openModelChoices(app)
        app.buttons["model.choice.fixture-model"].tap()
        dismissModelSettings(app)
        chooseFixtureEffort(app, "high")
        captureScreen(app, named: "Account switches preserve conversation and quick controls")
    }
}
