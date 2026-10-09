import XCTest

extension BexLaunchUITests {
    func testSimulatorMarksMergedWorktreesToTheRightOfRunningStatus() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        try useSimulatorListFixture("merge-worktree/fresh")
        refreshSimulatorTaskList(app)
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.waitForExistence(timeout: 10))
        project.tap()
        let running = app.descendants(matching: .any)["tasks.project.running.codex:merge-active"]
        XCTAssertTrue(running.waitForExistence(timeout: 10))
        let merged = app.descendants(matching: .any)["tasks.project.merged.codex:merge-active"]
        let unmerged = app.descendants(matching: .any)["tasks.project.unmerged.codex:merge-active"]
        XCTAssertFalse(merged.exists || unmerged.exists)
        try simulatorFixture("merge-worktree/dirty")
        refreshSimulatorTaskList(app)
        XCTAssertTrue(unmerged.waitForExistence(timeout: 10))
        XCTAssertTrue(running.exists)
        XCTAssertGreaterThan(unmerged.frame.minX, running.frame.maxX)
        try simulatorFixture("merge-worktree/clean")
        try simulatorFixture("merge-worktree/merged")
        refreshSimulatorTaskList(app)
        XCTAssertTrue(merged.waitForExistence(timeout: 10))
        XCTAssertFalse(unmerged.exists)
        XCTAssertTrue(running.exists)
        XCTAssertGreaterThan(merged.frame.minX, running.frame.maxX)
        XCTAssertTrue(app.descendants(matching: .any)["tasks.project.merged.codex:merge-idle"].exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Merged worktree session list"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.terminate()
        app.launch()
        expandSimulatorProject(app)
        XCTAssertTrue(merged.waitForExistence(timeout: 10))
        try simulatorFixture("merge-worktree/dirty")
        refreshSimulatorTaskList(app)
        XCTAssertTrue(unmerged.waitForExistence(timeout: 10))
        XCTAssertFalse(merged.exists)
        try simulatorFixture("merge-worktree/clean")
        refreshSimulatorTaskList(app)
        XCTAssertTrue(merged.waitForExistence(timeout: 10))
        try simulatorFixture("merge-worktree/new-work")
        refreshSimulatorTaskList(app)
        let removed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: merged)
        wait(for: [removed], timeout: 10)
        XCTAssertTrue(unmerged.exists)
        XCTAssertTrue(app.descendants(matching: .any)["tasks.project.unmerged.codex:merge-idle"].exists)
        XCTAssertTrue(running.exists)
    }

    private func refreshSimulatorTaskList(_ app: XCUIApplication) {
        let progress = app.descendants(matching: .any)["connection.progress"]
        let idle = NSPredicate(format: "exists == false")
        wait(for: [expectation(for: idle, evaluatedWith: progress)], timeout: 10)
        app.buttons["tasks.menu"].tap()
        let refresh = app.buttons["tasks.refresh"]
        XCTAssertTrue(refresh.isEnabled)
        refresh.tap()
        wait(for: [expectation(for: idle, evaluatedWith: progress)], timeout: 10)
    }

    func useAutomaticWorktrees(_ app: XCUIApplication) throws {
        openWorktreeSettings(app)
        let create = app.switches["worktree.create"]
        let original = try XCTUnwrap(create.value as? String)
        if original != "1" {
            toggle(create)
        }
        app.buttons["worktree.save"].tap()
        let saved = expectation(for: NSPredicate(format: "exists == false"),
                                evaluatedWith: app.buttons["worktree.save"])
        wait(for: [saved], timeout: 10)
        addTeardownBlock {
            let app = try self.connectedSimulatorApp(expandProject: false)
            self.openWorktreeSettings(app)
            let create = app.switches["worktree.create"]
            if create.value as? String != original {
                self.toggle(create)
            }
            app.buttons["worktree.save"].tap()
            let restored = self.expectation(for: NSPredicate(format: "exists == false"),
                                            evaluatedWith: app.buttons["worktree.save"])
            self.wait(for: [restored], timeout: 10)
        }
        app.terminate()
        _ = try connectedSimulatorApp()
        openWorktreeSettings(app)
        XCTAssertEqual(create.value as? String, "1", "The saved setting must survive app restart")
        app.buttons["worktree.cancel"].tap()
    }

    func testSimulatorEditsHostWorktreeSettingsFromTaskMenu() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        openWorktreeSettings(app)
        let create = app.switches["worktree.create"]
        let copy = app.switches["worktree.copy"]
        let deleteMerged = app.switches["worktree.deleteMerged"]
        let directory = app.textFields["worktree.directory"]
        let paths = app.textViews["worktree.paths"]
        XCTAssertEqual(create.value as? String, "0")
        XCTAssertEqual(copy.value as? String, "0")
        toggle(create); toggle(copy)
        XCTAssertEqual(create.value as? String, "1")
        XCTAssertEqual(copy.value as? String, "1")
        scrollToListElement(deleteMerged, in: app)
        XCTAssertEqual(deleteMerged.value as? String, "0")
        toggle(deleteMerged)
        XCTAssertEqual(deleteMerged.value as? String, "1")
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
        scrollToListElement(deleteMerged, in: app)
        XCTAssertEqual(deleteMerged.value as? String, "1")
        let saved = XCTAttachment(screenshot: app.screenshot())
        saved.name = "Host worktree settings reopened on iPhone"; saved.lifetime = .keepAlways; add(saved)
        verifyWorktreeCancellationAndReset(app)
    }

    private func openWorktreeSettings(_ app: XCUIApplication) {
        let worktrees = app.buttons["settings.worktrees"]
        if !worktrees.exists {
            let menu = app.buttons["tasks.menu"]
            let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: menu)
            wait(for: [visible], timeout: 10)
            menu.tap()
            app.buttons["tasks.settings"].tap()
        }
        XCTAssertTrue(worktrees.waitForExistence(timeout: 10)); worktrees.tap()
        let save = app.buttons["worktree.save"]
        XCTAssertTrue(save.waitForExistence(timeout: 10))
        let loaded = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: save)
        wait(for: [loaded], timeout: 10)
    }

    private func verifyWorktreeCancellationAndReset(_ app: XCUIApplication) {
        let create = app.switches["worktree.create"]
        let copy = app.switches["worktree.copy"]
        let deleteMerged = app.switches["worktree.deleteMerged"]
        let directory = app.textFields["worktree.directory"]
        scrollToEarlierListElement(create, in: app, attempts: 10)
        toggle(create)
        scrollToListElement(deleteMerged, in: app)
        toggle(deleteMerged)
        app.buttons["worktree.cancel"].tap()
        openWorktreeSettings(app)
        XCTAssertEqual(create.value as? String, "1", "Cancel must not update Host preferences")
        scrollToListElement(deleteMerged, in: app)
        XCTAssertEqual(deleteMerged.value as? String, "1", "Cancel must not update cleanup preferences")
        scrollToEarlierListElement(create, in: app, attempts: 10)
        toggle(create); toggle(copy)
        scrollToListElement(deleteMerged, in: app)
        toggle(deleteMerged)
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
        scrollToListElement(deleteMerged, in: app)
        XCTAssertEqual(deleteMerged.value as? String, "0")
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
        let row = app.descendants(matching: .any)["tasks.row.codex:fixture-worktree-thread"]
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
        try openDiffAfterRetry(app)
        assertWorkspaceDiffTabs(app)
        closeWorkbench(app)
        XCTAssertTrue(changes.waitForExistence(timeout: 10))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        openFiles(app)
        XCTAssertTrue(app.buttons["files.open-path"].waitForExistence(timeout: 10))
        app.buttons["変更済み"].tap()
        XCTAssertTrue(app.staticTexts["+Session worktree first"].waitForExistence(timeout: 10))
        closeWorkbench(app)
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

    private func assertWorkspaceDiffTabs(_ app: XCUIApplication) {
        XCTAssertTrue(app.staticTexts["+Session worktree first"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["files.open-path"].exists)
        let card = app.buttons["diff.file.tracked.txt"]
        card.tap()
        XCTAssertFalse(app.staticTexts["+Session worktree first"].exists)
        card.tap()
        XCTAssertTrue(app.staticTexts["+Session worktree first"].waitForExistence(timeout: 10))
        app.buttons["すべてのファイル"].tap()
        XCTAssertTrue(app.buttons["files.open-path"].waitForExistence(timeout: 10))
        app.buttons["変更済み"].tap()
        XCTAssertTrue(app.staticTexts["+Session worktree first"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["+Session worktree second"].exists)
        XCTAssertTrue(app.staticTexts["-original"].exists)
        XCTAssertFalse(app.staticTexts["+Project-only change"].exists)
        captureScreen(app, named: "Tabbed session worktree diff")
    }

    private func openDiffAfterRetry(_ app: XCUIApplication) throws {
        try simulatorFixture("worktree/unavailable")
        app.buttons["task.diff"].tap()
        let retry = app.buttons["files.diff.retry"]
        XCTAssertTrue(retry.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["files.open-path"].exists)
        try simulatorFixture("worktree/restore")
        retry.tap()
    }
}
