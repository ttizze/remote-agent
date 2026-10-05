import XCTest

extension BexLaunchUITests {
    func testSimulatorModelDefaultsInheritAndPersistAcrossScopes() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15)); choice.tap()
        app.buttons["model.option.reasoningEffort"].tap(); app.buttons["medium"].tap()
        app.buttons["settings.scope.environment"].tap()
        app.buttons["settings.scope.environment.current"].tap()
        app.buttons["model.option.reasoningEffort"].tap(); app.buttons["high"].tap()
        app.buttons["model.option.serviceTier"].tap(); app.buttons["高速"].tap()
        app.buttons["settings.scope.projects"].tap()
        app.buttons["settings.scope.projects.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.option.reasoningEffort"].value as? String, "high")
        app.buttons["model.option.reasoningEffort"].tap(); app.buttons["medium"].tap()
        XCTAssertTrue(app.buttons["model.defaults.inherit"].exists)
        captureScreen(app, named: "Project model defaults with environment inheritance")
        app.buttons["model.defaults.inherit"].tap()
        XCTAssertEqual(app.buttons["model.option.reasoningEffort"].value as? String, "high")
        XCTAssertFalse(app.buttons["model.defaults.inherit"].exists)
        dismissModelSettings(app); app.buttons["tasks.new.project.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オン")
        app.buttons["BackButton"].tap()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        app.buttons["settings.scope.projects"].tap()
        app.buttons["settings.scope.projects.simulator-project"].tap()
        app.buttons["model.option.reasoningEffort"].tap(); app.buttons["medium"].tap()
        dismissModelSettings(app); app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap(); XCTAssertEqual(
            app.buttons["model.option.reasoningEffort"].value as? String,
            "high"
        )
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.menu"].waitForExistence(timeout: 20))
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        app.buttons["settings.models"].tap()
        app.buttons["settings.scope.projects"].tap()
        app.buttons["settings.scope.projects.simulator-project"].tap()
        XCTAssertEqual(app.buttons["model.option.reasoningEffort"].value as? String, "medium")
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
                          app.buttons["model.option.reasoningEffort"].frame.minY)
        XCTAssertEqual(
            app.buttons["model.option.reasoningEffort"].frame.midY,
            app.buttons["model.option.serviceTier"].frame.midY,
            accuracy: 1
        )
        XCTAssertLessThanOrEqual(
            app.buttons["model.option.reasoningEffort"].frame.maxX,
            app.buttons["model.option.serviceTier"].frame.minX
        )
        captureScreen(app, named: "Agent icon rail with account weekly quota models and compact controls")
        dismissModelSettings(app)
    }
}
