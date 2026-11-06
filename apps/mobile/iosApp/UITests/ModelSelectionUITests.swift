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
}
