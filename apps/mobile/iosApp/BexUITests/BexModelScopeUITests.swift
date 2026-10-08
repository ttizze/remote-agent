import XCTest

extension BexLaunchUITests {
    func testSimulatorModelDefaultsInheritAndPersistAcrossScopes() throws {
        let app = try connectedSimulatorApp()
        openDefaultModelSettings(app)
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15)); choice.tap()
        chooseDefaultModelMenuOption(app, menu: "model.sheet.effort", option: "medium")
        chooseDefaultModelMenuOption(
            app,
            menu: "settings.scope.environment",
            option: "settings.scope.environment.current"
        )
        chooseDefaultModelMenuOption(app, menu: "model.sheet.effort", option: "high")
        chooseDefaultModelMenuOption(app, menu: "model.defaults.speed", option: "高速")
        chooseDefaultModelMenuOption(
            app,
            menu: "settings.scope.projects",
            option: "settings.scope.projects.simulator-project"
        )
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        chooseDefaultModelMenuOption(app, menu: "model.sheet.effort", option: "medium")
        XCTAssertTrue(app.buttons["model.defaults.inherit"].exists)
        captureScreen(app, named: "Project model defaults with environment inheritance")
        app.buttons["model.defaults.inherit"].tap()
        XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        XCTAssertFalse(app.buttons["model.defaults.inherit"].exists)
        dismissModelSettings(app); app.buttons["tasks.new.project.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オン")
        app.buttons["BackButton"].tap()
        openDefaultModelSettings(app)
        chooseDefaultModelMenuOption(
            app,
            menu: "settings.scope.projects",
            option: "settings.scope.projects.simulator-project"
        )
        chooseDefaultModelMenuOption(app, menu: "model.sheet.effort", option: "medium")
        dismissModelSettings(app); app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap(); XCTAssertEqual(app.buttons["model.sheet.effort"].value as? String, "high")
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.menu"].waitForExistence(timeout: 20))
        openDefaultModelSettings(app)
        chooseDefaultModelMenuOption(
            app,
            menu: "settings.scope.projects",
            option: "settings.scope.projects.simulator-project"
        )
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
        openDefaultModelSettings(app)
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15)); choice.tap()
        chooseIndependentNewChatModel(app, choice: choice)
        XCTAssertFalse(app.descendants(matching: .any)["model.error"].exists)
        chooseDefaultModelMenuOption(app, menu: "model.sheet.effort", option: "high")
        chooseDefaultModelMenuOption(app, menu: "model.defaults.speed", option: "高速")
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
        openDefaultModelSettings(app)
        assertPersistedIndependentModelDefaults(app)
    }

    private func openDefaultModelSettings(_ app: XCUIApplication) {
        chooseDefaultModelMenuOption(app, menu: "tasks.menu", option: "tasks.settings")
        let models = app.buttons["settings.models"]
        XCTAssertTrue(models.waitForExistence(timeout: 10))
        waitForStableFrame(models)
        models.tap()
        XCTAssertTrue(app.buttons["model.choice.automatic"].waitForExistence(timeout: 10))
    }

    private func chooseDefaultModelMenuOption(_ app: XCUIApplication, menu: String, option: String) {
        let control = app.buttons[menu]
        XCTAssertTrue(control.waitForExistence(timeout: 10))
        waitForStableFrame(control)
        control.tap()
        let choice = app.buttons[option]
        XCTAssertTrue(choice.waitForExistence(timeout: 10))
        waitForStableFrame(choice)
        choice.tap()
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
        chooseDefaultModelMenuOption(
            app,
            menu: "model.defaults.new-chat",
            option: "model.defaults.new-chat.fixture-model"
        )
        XCTAssertEqual(newChat.value as? String, "Fixture Model")
        app.buttons["model.provider.claude"].tap()
        XCTAssertEqual(app.buttons["model.choice.automatic"].value as? String, "選択中")
        XCTAssertEqual(newChat.value as? String, "Fixture Model")
        app.buttons["model.provider.codex"].tap()
        XCTAssertEqual(choice.value as? String, "選択中")
    }
}
