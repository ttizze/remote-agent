import XCTest

extension BexLaunchUITests {
    func testSimulatorEditsHostWorktreeSettingsFromTaskMenu() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        openWorktreeSettings(app)
        let create = app.switches["worktree.create"]
        let copy = app.switches["worktree.copy"]
        let directory = app.textFields["worktree.directory"]
        let paths = app.textViews["worktree.paths"]
        XCTAssertEqual(create.value as? String, "0")
        XCTAssertEqual(copy.value as? String, "0")
        toggle(create); toggle(copy)
        XCTAssertEqual(create.value as? String, "1")
        XCTAssertEqual(copy.value as? String, "1")
        paths.tap(); paths.typeText(".env\nconfig/local")
        directory.tap(); directory.typeText("relative")
        app.buttons["worktree.save"].tap()
        XCTAssertTrue(app.staticTexts["worktree.error"].waitForExistence(timeout: 10))
        XCTAssertEqual(directory.value as? String, "relative")
        XCTAssertEqual(create.value as? String, "1")
        let destination = "/tmp/bex-worktree-ui-\(UUID().uuidString)"
        directory.tap()
        directory.typeKey("a", modifierFlags: .command)
        directory.typeText(destination)
        app.buttons["worktree.save"].tap()
        let savedAndClosed = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: app.buttons["worktree.save"]
        )
        wait(for: [savedAndClosed], timeout: 10)
        app.terminate()
        _ = try connectedSimulatorApp()
        openWorktreeSettings(app)
        XCTAssertEqual(create.value as? String, "1")
        XCTAssertEqual(copy.value as? String, "1")
        XCTAssertEqual(directory.value as? String, destination)
        XCTAssertEqual(paths.value as? String, ".env\nconfig/local")
        let saved = XCTAttachment(screenshot: app.screenshot())
        saved.name = "Host worktree settings reopened on iPhone"; saved.lifetime = .keepAlways; add(saved)
        verifyWorktreeCancellationAndReset(app)
    }

    private func openWorktreeSettings(_ app: XCUIApplication) {
        let menu = app.buttons["tasks.menu"]
        let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: menu)
        wait(for: [visible], timeout: 10)
        menu.tap()
        app.buttons["tasks.worktree-settings"].tap()
        let save = app.buttons["worktree.save"]
        XCTAssertTrue(save.waitForExistence(timeout: 10))
        let loaded = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: save)
        wait(for: [loaded], timeout: 10)
    }

    private func verifyWorktreeCancellationAndReset(_ app: XCUIApplication) {
        let create = app.switches["worktree.create"]
        let copy = app.switches["worktree.copy"]
        let directory = app.textFields["worktree.directory"]
        toggle(create)
        app.buttons["worktree.cancel"].tap()
        openWorktreeSettings(app)
        XCTAssertEqual(create.value as? String, "1", "Cancel must not update Host preferences")
        toggle(create); toggle(copy)
        directory.tap()
        directory.press(forDuration: 1.2)
        let selectAll = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == 'Select All' OR label == 'すべてを選択'")).firstMatch
        XCTAssertTrue(selectAll.waitForExistence(timeout: 5))
        selectAll.tap()
        directory.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertEqual(directory.value as? String, directory.placeholderValue)
        app.buttons["worktree.save"].tap()
        openWorktreeSettings(app)
        XCTAssertEqual(create.value as? String, "0")
        XCTAssertEqual(copy.value as? String, "0")
        XCTAssertEqual(directory.value as? String, directory.placeholderValue)
        app.buttons["worktree.cancel"].tap()
    }

    private func toggle(_ control: XCUIElement) {
        // SwiftUI exposes the label and switch as one wide accessibility element.
        control.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5)).tap()
    }
}
