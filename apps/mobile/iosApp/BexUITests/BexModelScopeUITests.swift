import XCTest

extension BexLaunchUITests {
    func testSimulatorModelDefaultsInheritAndPersistAcrossScopes() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15)); choice.tap()
        app.buttons["model.sheet.effort"].tap(); app.buttons["medium"].tap()
        app.buttons["settings.scope.environment"].tap()
        app.buttons["settings.scope.environment.current"].tap()
        app.buttons["model.sheet.effort"].tap(); app.buttons["high"].tap()
        app.buttons["model.defaults.speed"].tap(); app.buttons["高速"].tap()
        app.buttons["settings.scope.projects"].tap()
        app.buttons["settings.scope.projects.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        app.buttons["model.sheet.effort"].tap(); app.buttons["medium"].tap()
        XCTAssertTrue(app.buttons["model.defaults.inherit"].exists)
        captureScreen(app, named: "Project model defaults with environment inheritance")
        app.buttons["model.defaults.inherit"].tap()
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        XCTAssertFalse(app.buttons["model.defaults.inherit"].exists)
        dismissModelSettings(app); app.buttons["tasks.new.project.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オン")
        app.buttons["BackButton"].tap()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        app.buttons["settings.scope.projects"].tap()
        app.buttons["settings.scope.projects.simulator-project"].tap()
        app.buttons["model.sheet.effort"].tap(); app.buttons["medium"].tap()
        dismissModelSettings(app); app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap(); XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.menu"].waitForExistence(timeout: 20))
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        app.buttons["settings.scope.projects"].tap()
        app.buttons["settings.scope.projects.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "medium")
    }

    func testSimulatorModelPickerUsesAgentRailAndCompactControls() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        app.buttons["model.settings"].tap()
        XCTAssertFalse(app.buttons["settings.scope.projects"].exists || app.buttons["settings.scope.environment"]
            .exists)
        let weekly = app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "週間残量 86%"))
            .firstMatch
        XCTAssertTrue(weekly.waitForExistence(timeout: 15)); XCTAssertTrue(weekly.isHittable)
        XCTAssertFalse(app.staticTexts["5時間枠"].exists || app.staticTexts["72%"].exists)
        let codex = app.buttons["model.provider.codex"]
        let claude = app.buttons["model.provider.claude"]
        XCTAssertLessThanOrEqual(codex.frame.maxX, app.buttons["model.accounts.manage"].frame.minX)
        XCTAssertLessThanOrEqual(codex.frame.maxY, claude.frame.minY)
        XCTAssertEqual(codex.value as? String, "選択中")
        XCTAssertFalse(app.buttons["model.close"].exists)
        XCTAssertFalse(app.staticTexts["思考の深さ"].exists || app.staticTexts["速度"].exists)
        XCTAssertLessThan(weekly.frame.maxY, app.textFields["model.search"].frame.minY)
        XCTAssertLessThan(app.buttons["model.choice.fixture-model"].frame.maxY,
                          app.buttons["model.sheet.effort"].frame.minY)
        XCTAssertEqual(
            app.buttons["model.sheet.effort"].frame.midY,
            app.buttons["model.sheet.fast"].frame.midY,
            accuracy: 1
        )
        XCTAssertLessThanOrEqual(
            app.buttons["model.sheet.effort"].frame.maxX,
            app.buttons["model.sheet.fast"].frame.minX
        )
        captureScreen(app, named: "Agent icon rail with account weekly quota models and compact controls")
        dismissModelSettings(app)
    }

    func testSimulatorModelDefaultsPersistAndApplyOnlyToNewConversations() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        chooseFixtureEffort(app, "medium")
        let fast = app.buttons["model.fast"]
        if fast.value as? String == "オン" {
            fast.tap()
        }
        XCTAssertEqual(fast.value as? String, "オフ")
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
        chooseIndependentNewChatModel(app, choice: choice)
        XCTAssertFalse(app.descendants(matching: .any)["model.error"].exists)
        let effort = app.buttons["model.sheet.effort"]
        XCTAssertTrue(effort.waitForExistence(timeout: 10)); effort.tap()
        app.buttons["high"].tap()
        let speed = app.buttons["model.defaults.speed"]
        XCTAssertTrue(speed.exists); speed.tap()
        app.buttons["高速"].tap()
        captureScreen(app, named: "Default model effort and speed for new conversations")
        dismissModelSettings(app)
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
        dismissModelSettings(app)
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.menu"].waitForExistence(timeout: 20))
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        assertPersistedIndependentModelDefaults(app)
    }

    private func assertPersistedIndependentModelDefaults(_ app: XCUIApplication) {
        XCTAssertEqual(app.buttons["model.choice.fixture-model"].value as? String, "選択中")
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.defaults.speed"].value as? String, "高速")
        XCTAssertEqual(app.buttons["model.defaults.new-chat"].value as? String, "Fixture Model")
        app.buttons["model.choice.automatic"].tap()
        XCTAssertEqual(app.buttons["model.choice.automatic"].value as? String, "選択中")
        XCTAssertEqual(app.buttons["model.defaults.new-chat"].value as? String, "Fixture Model")
    }

    private func chooseIndependentNewChatModel(_ app: XCUIApplication, choice: XCUIElement) {
        let newChat = app.buttons["model.defaults.new-chat"]
        XCTAssertTrue(newChat.exists); newChat.tap()
        app.buttons["model.defaults.new-chat.fixture-model"].tap()
        XCTAssertEqual(newChat.value as? String, "Fixture Model")
        app.buttons["model.provider.claude"].tap()
        XCTAssertEqual(app.buttons["model.choice.automatic"].value as? String, "選択中")
        XCTAssertEqual(newChat.value as? String, "Fixture Model")
        app.buttons["model.provider.codex"].tap()
        XCTAssertEqual(choice.value as? String, "選択中")
    }
}
