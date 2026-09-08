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

    func testSimulatorReviewsTheOpenSessionsWorktree() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        try useSimulatorListFixture("worktree-conversation")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.waitForExistence(timeout: 10)); project.tap()
        let row = app.descendants(matching: .any)["tasks.row.fixture-worktree-thread"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        let changes = app.buttons["task.diff"]
        XCTAssertTrue(changes.waitForExistence(timeout: 15))
        let staleWorkspace = expectation(for: NSPredicate(format: "exists == true"),
                                         evaluatedWith: app.staticTexts["検証プロジェクト · 検証 Mac"])
        staleWorkspace.isInverted = true
        wait(for: [staleWorkspace], timeout: 3)
        XCTAssertTrue(changes.label.contains("1件のファイル"), changes.label)
        XCTAssertTrue(changes.label.contains("+2"), changes.label)
        XCTAssertTrue(changes.label.contains("−1"), changes.label)
        openFiles(app)
        let diff = app.buttons["files.diff"]
        XCTAssertTrue(diff.waitForExistence(timeout: 10)); diff.tap()
        XCTAssertTrue(app.staticTexts["+Session worktree first"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["+Session worktree second"].exists)
        XCTAssertTrue(app.staticTexts["-original"].exists)
        XCTAssertFalse(app.staticTexts["+Project-only change"].exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Open session worktree diff"
        screenshot.lifetime = .keepAlways; add(screenshot)
        app.buttons["files.diff.close"].tap()
        app.buttons["files.close"].tap()
        let input = app.descendants(matching: .any)["task.message"]
        input.tap(); input.typeText("[workspace-edit] Update the worktree while running")
        app.buttons["task.send"].tap()
        let running = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(running.waitForExistence(timeout: 10))
        let updated = expectation(
            for: NSPredicate(format: "label CONTAINS %@ AND label CONTAINS %@ AND label CONTAINS %@",
                             "2件のファイル", "+4", "−1"),
            evaluatedWith: changes
        )
        wait(for: [updated], timeout: 20)
        XCTAssertTrue(running.exists, "Counts must update before the turn finishes")
        captureScreen(app, named: "Live worktree counts before turn completion")
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 10))
    }
}
