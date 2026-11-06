import XCTest

extension LaunchUITests {
    func testSimulatorModelPickerUsesNativeFormAndAgentPicker() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        app.buttons["model.settings"].tap()
        XCTAssertFalse(app.buttons["settings.scope.projects"].exists || app.buttons["settings.scope.environment"]
            .exists)
        let picker = app.segmentedControls["model.provider"]
        let codex = picker.buttons["Codex"]
        let claude = picker.buttons["Claude Code"]
        XCTAssertTrue(codex.isSelected)
        XCTAssertEqual(codex.frame.midY, claude.frame.midY, accuracy: 1)
        XCTAssertTrue(app.navigationBars["モデル設定"].exists)
        XCTAssertTrue(app.buttons["model.close"].exists)
        XCTAssertTrue(app.searchFields.firstMatch.exists)
        let effort = app.buttons["model.sheet.effort"]
        let speed = app.buttons["model.sheet.fast"]
        let account = app.buttons["model.accounts.manage"]
        scrollToListElement(account, in: app, bottomMargin: 0)
        XCTAssertLessThan(effort.frame.maxY, speed.frame.minY)
        XCTAssertLessThan(speed.frame.maxY, account.frame.minY)
        let weekly = app.descendants(matching: .any).matching(NSPredicate(
            format: "label == %@", "週間枠 残り 86%"
        )).firstMatch
        scrollToListElement(weekly, in: app, bottomMargin: 0)
        XCTAssertTrue(weekly.exists)
        XCTAssertTrue(weekly.isHittable)
        XCTAssertFalse(app.staticTexts["5時間枠"].exists)
        captureScreen(app, named: "Model picker with accounts below response settings")
        dismissModelSettings(app)
    }

    func testSimulatorLastModelSelectionPersistsAndKeepsExistingDrafts() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        chooseFixtureModel(app)
        let fast = app.buttons["model.fast"]
        if fast.value as? String != "オン" {
            fast.tap()
        }
        XCTAssertEqual(fast.value as? String, "オン", app.debugDescription)
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("Keep this existing draft")
        app.buttons["BackButton"].tap()
        app.buttons["tasks.new.chat"].tap()
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オン")
        chooseFixtureEffort(app, "medium")
        app.buttons["model.fast"].tap()
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オフ")
        app.buttons["BackButton"].tap()
        app.buttons["tasks.new.project.simulator-project"].tap()
        XCTAssertEqual(app.textFields["task.message"].value as? String, "Keep this existing draft")
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オン")
        app.buttons["BackButton"].tap()
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.menu"].waitUntilExists(timeout: 20))
        app.buttons["tasks.new.chat"].tap()
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "medium")
        XCTAssertEqual(app.buttons["model.fast"].value as? String, "オフ")
        captureScreen(app, named: "Last model choices restored without changing existing drafts")
    }

    func testSimulatorWeeklyUsageNamesFableAndSettingsOmitModelDefaults() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.settings"].tap()
        XCTAssertTrue(app.buttons["settings.providers"].waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["settings.models"].exists)
        app.buttons["settings.close"].tap()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        openModelChoices(app)
        let weekly = app.descendants(matching: .any).matching(NSPredicate(
            format: "label == %@", "Fable · 週間枠 残り 66%"
        )).firstMatch
        scrollToListElement(weekly, in: app, bottomMargin: 0)
        XCTAssertTrue(weekly.exists)
        XCTAssertTrue(weekly.isHittable)
        captureScreen(app, named: "Weekly usage keeps the Fable scope name")
    }
}
