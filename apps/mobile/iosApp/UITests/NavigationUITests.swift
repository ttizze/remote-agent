import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    private func openConnectionSettings(_ app: XCUIApplication) {
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.settings"].tap()
        XCTAssertTrue(app.buttons["settings.providers"].waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["settings.models"].exists)
        app.buttons["settings.connections"].tap()
        XCTAssertTrue(app.buttons["profiles.add"].waitUntilExists(timeout: 10))
    }

    func testSimulatorRemovesHostAndRequiresPairingAfterRelaunch() throws {
        let app = try connectedSimulatorApp()
        openConnectionSettings(app)
        let profile = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH %@ AND identifier != %@", "profiles.", "profiles.add"
        )).firstMatch
        XCTAssertTrue(profile.waitUntilExists(timeout: 10))
        captureScreen(app, named: "Redesigned saved PC screen")
        profile.press(forDuration: 1)
        let remove = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "connection.remove."))
            .firstMatch
        XCTAssertTrue(remove.waitUntilExists(timeout: 10))
        captureScreen(app, named: "PC list connection removal")
        remove.tap()
        app.alerts.buttons["キャンセル"].tap()
        profile.press(forDuration: 1)
        XCTAssertTrue(remove.waitUntilExists(timeout: 5))
        remove.tap()
        app.alerts.buttons["接続を解除"].tap()
        app.navigationBars["環境"].buttons["BackButton"].tap()
        app.buttons["settings.close"].tap()
        XCTAssertTrue(app.buttons["tasks.connect"].waitUntilExists(timeout: 10))
        app.buttons["tasks.connect"].tap()
        XCTAssertTrue(app.staticTexts["pairing.welcome"].waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["pairing.scan"].exists)
        XCTAssertEqual(app.buttons["pairing.scan"].label, "QRコードを読み取る")
        assertUnavailableScannerCanBeCancelled(app)
        XCTAssertFalse(app.buttons["tasks.project.simulator-project"].exists)
        app.terminate(); app.launch()
        XCTAssertTrue(app.buttons["tasks.connect"].waitUntilExists(timeout: 10))
        app.buttons["tasks.connect"].tap()
        XCTAssertTrue(app.staticTexts["pairing.welcome"].waitUntilExists(timeout: 10))
        app.buttons["privacy.policy"].tap()
        XCTAssertTrue(app.navigationBars["プライバシーポリシー"].waitUntilExists(timeout: 5))
        app.buttons["閉じる"].tap()
        XCTAssertTrue(app.buttons["pairing.scan"].waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["tasks.project.simulator-project"].exists)
        captureScreen(app, named: "Removed Host stays unpaired after relaunch")
        app.terminate()
        let reconnected = try connectedSimulatorApp()
        XCTAssertTrue(reconnected.buttons["tasks.project.simulator-project"].exists)
        XCTAssertFalse(reconnected.staticTexts["notice"].exists)
    }

    func testSimulatorStartsOnListAndPreservesDetailOnForeground() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let projectCompose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(projectCompose.waitUntilExists(timeout: 10)); projectCompose.tap()
        chooseFixtureModel(app)
        let prompt = app.descendants(matching: .any)["task.message"]
        prompt.tap(); prompt.typeText("[model] Verify chosen settings")
        app.buttons["task.send"].tap()
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitUntilExists(timeout: 25), "Chosen settings were rejected by the fixture")
        let finalID = finalAnswer.identifier
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.exists)
        app.terminate(); app.launch()
        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitUntilExists(timeout: 30))
        XCTAssertFalse(detail.exists)
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.waitUntilExists(timeout: 10))
        XCTAssertEqual(project.value as? String, "閉じています")
        project.tap()
        let threadNumber = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(threadNumber)")]
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitUntilExists(timeout: 30))
        let settings = app.buttons["model.settings"]
        verifyRestoredModelSettings(app, settings: settings)
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(detail.waitUntilExists(timeout: 15))
        captureScreen(app, named: "Open conversation retained on foreground")
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitUntilExists(timeout: 20))
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitUntilExists(timeout: 10)); composer.tap()
        composer.typeText("[model] Verify settings after restart")
        let send = app.buttons["task.send"]
        let ready = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        wait(for: [ready], timeout: 30); send.tap()
        let nextAnswer = app.descendants(matching: .any)[String(finalID.dropLast()) + "2"]
        XCTAssertTrue(nextAnswer.waitUntilExists(timeout: 25))
        composer.tap()
        XCTAssertTrue(settings.isHittable)
        captureScreen(app, named: "Restored task and selected model")
    }

    func testSimulatorOpensQrScannerWhileAddingHost() throws {
        let app = try connectedSimulatorApp()
        openConnectionSettings(app)
        XCTAssertTrue(app.buttons["pairing.scan"].exists)
        XCTAssertTrue(app.buttons["pairing.manual"].exists)
        XCTAssertFalse(app.sheets.firstMatch.exists)
        app.buttons["profiles.add"].tap()
        allowFirstSystemPermissionIfPresent()
        XCTAssertTrue(app.staticTexts["scanner.error"].waitUntilExists(timeout: 10))
        app.buttons["scanner.cancel"].tap()
        assertUnavailableScannerCanBeCancelled(app)
        XCTAssertTrue(app.buttons["profiles.add"].isHittable)
        XCTAssertTrue(app.buttons["pairing.scan"].isHittable)
    }

    func testSimulatorUsesNativeHostNavigationAndPairingDismissal() throws {
        let app = try connectedSimulatorApp()
        let selector = app.buttons["tasks.connections"]
        XCTAssertTrue(selector.waitUntilExists(timeout: 10))
        assertConnectionScopeSwitches(app)
        openConnectionSettings(app)
        XCTAssertFalse(app.sheets.firstMatch.exists)
        XCTAssertTrue(app.buttons["pairing.manual"].exists)
        let host = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH %@ AND label CONTAINS %@", "profiles.", "検証 Host"
        )).firstMatch
        XCTAssertTrue(host.waitUntilExists(timeout: 10)); host.tap()
        XCTAssertTrue(app.switches["environment.enabled"].waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["providers.refresh"].exists)
        app.navigationBars["検証 Host"].buttons["BackButton"].tap()
        app.navigationBars["環境"].buttons["BackButton"].tap()
        app.buttons["settings.close"].tap()
        XCTAssertTrue(selector.waitUntilExists(timeout: 10))

        openConnectionSettings(app)
        app.buttons["pairing.manual"].tap()
        let contents = app.secureTextFields["pairing.contents"]
        contents.tap(); contents.typeText("invalid invitation")
        app.buttons["pairing.submit"].tap()
        XCTAssertTrue(app.staticTexts["notice"].waitUntilExists(timeout: 5))
        // The error can appear before the native keyboard finishes dismissing.
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))
        contents.tap()
        let payload = try simulatorPairingPayload()
        contents.typeText(payload)
        app.buttons["pairing.submit"].tap()
        XCTAssertTrue(app.buttons["pairing.change"].waitUntilExists(timeout: 10))
        app.buttons["pairing.change"].tap()
        submitManualPairing(app, payload: payload)
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 30))
        XCTAssertFalse(app.navigationBars["環境"].exists)
        XCTAssertTrue(app.buttons["tasks.connections"].label.contains("検証 Host"))
        app.terminate(); app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 30))
        XCTAssertTrue(app.buttons["tasks.connections"].label.contains("検証 Host"))
    }

    private func assertConnectionScopeSwitches(_ app: XCUIApplication) {
        let selector = app.buttons["tasks.connections"]
        selector.tap()
        app.buttons["tasks.connections.all"].tap()
        XCTAssertTrue(selector.label.contains("すべての接続先"))
        selector.tap()
        app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND label CONTAINS %@",
                                         "tasks.connections.", "検証 Host")).firstMatch.tap()
    }

    func testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitUntilExists(timeout: 10))
        XCTAssertGreaterThan(search.frame.midY, app.frame.height * 0.8)
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.exists)
        // Expansion persists across launches; this scenario begins with a closed project.
        if project.value as? String == "開いています" {
            project.tap()
        }
        XCTAssertEqual(project.value as? String, "閉じています")
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.isHittable)
        captureScreen(app, named: "Remote projects with bottom search")
        compose.tap()
        XCTAssertTrue(app.textFields["task.message"].waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["tasks.new.chat"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(search.waitUntilExists(timeout: 10))
        XCTAssertEqual(project.value as? String, "閉じています")
        search.tap(); search.typeText("no-matching-conversation-unique\n")
        XCTAssertFalse(app.buttons["tasks.project.simulator-project"].exists)
        search.buttons.firstMatch.tap()
        closeSearch(app)
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(project.waitUntilExists(timeout: 10))
        project.tap()
        captureScreen(app, named: "Expanded project rows")
        XCTAssertEqual(project.value as? String, "開いています")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertEqual(project.value as? String, "開いています")
        compose.tap()
        XCTAssertTrue(app.textFields["task.message"].waitUntilExists(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertEqual(project.value as? String, "開いています")
        app.buttons["tasks.new.chat"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitUntilExists(timeout: 10))
    }

    func testSimulatorUsesClaudeWithoutCodexAndRestoresConversation() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        app.buttons["tasks.new.chat"].tap()
        XCTAssertTrue(app.buttons["model.settings"].waitUntilExists(timeout: 10))
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        openModelChoices(app)
        let choice = app.buttons["model.choice.default"]
        XCTAssertTrue(choice.waitUntilExists(timeout: 15))
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        choice.tap()
        captureScreen(app, named: "Claude models without Codex")
        dismissModelSettings(app)
        let message = app.textFields["task.message"]
        message.tap()
        message.typeText("Claude standalone")
        app.buttons["task.send"].tap()
        let answer = app.textViews["reply 1: Claude standalone"]
        XCTAssertTrue(answer.waitUntilExists(timeout: 30))
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        app.terminate()
        app.launch()
        let row = prefixedElement(app, prefix: "tasks.row.claude:")
        XCTAssertTrue(row.waitUntilExists(timeout: 30)); row.tap()
        XCTAssertTrue(answer.waitUntilExists(timeout: 20))
        message.tap()
        message.typeText("Follow up")
        app.buttons["task.send"].tap()
        XCTAssertTrue(app.textViews["reply 2: Follow up"].waitUntilExists(timeout: 30))
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Claude conversation restored and continued")
    }

    func openModelChoices(_ app: XCUIApplication) {
        if app.buttons["model.account.add"].exists {
            app.navigationBars.buttons["BackButton"].firstMatch.tap()
        }
        XCTAssertTrue(app.searchFields.firstMatch.waitUntilExists(timeout: 15))
    }

    func dismissModelSettings(_ app: XCUIApplication) {
        let surface = app.buttons["model.close"]
        tapWhenStable(surface)
        let dismissed = expectation(for: NSPredicate(format: "exists == false"),
                                    evaluatedWith: surface)
        wait(for: [dismissed], timeout: 10)
    }

    /// An idle reply composer shows its model controls once focused.
    func expandComposer(_ app: XCUIApplication) {
        let settings = app.buttons["model.settings"]
        if !settings.exists {
            app.textFields["task.message"].tap()
            XCTAssertTrue(settings.waitUntilExists(timeout: 5))
        }
    }

    func chooseFixtureEffort(_ app: XCUIApplication, _ value: String) {
        expandComposer(app)
        let control = app.buttons["model.effort"]
        XCTAssertTrue(control.waitUntilExists(timeout: 10)); control.tap()
        app.buttons["model.effort." + value].tap()
    }

    func chooseFixtureModel(_ app: XCUIApplication) {
        expandComposer(app)
        tapWhenStable(app.buttons["model.settings"])
        selectFixtureProvider(app, "Codex")
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitUntilExists(timeout: 10)); choice.tap()
        dismissModelSettings(app)
        chooseFixtureEffort(app, "high")
    }

    func verifyRestoredModelSettings(_ app: XCUIApplication, settings: XCUIElement) {
        expandComposer(app)
        XCTAssertTrue(settings.waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertGreaterThan(settings.frame.midY, app.descendants(matching: .any)["task.message"].frame.midY)
        settings.tap()
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitUntilExists(timeout: 10))
        XCTAssertTrue(choice.isSelected)
        captureScreen(app, named: "Restored model and account picker")
        dismissModelSettings(app)
        expandComposer(app)
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "high")
        chooseFixtureEffort(app, "medium")
        XCTAssertEqual(app.buttons["model.effort"].value as? String, "medium")
        chooseFixtureEffort(app, "high")
    }

    func testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let projectCompose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(projectCompose.waitUntilExists(timeout: 10)); projectCompose.tap()
        let draft = app.textFields["task.message"]
        XCTAssertTrue(draft.waitUntilExists(timeout: 10))
        draft.tap(); draft.typeText("[success] Retain this project draft")
        backToTaskList(app, swipe: true)
        projectCompose.tap()
        XCTAssertTrue(draft.waitUntilExists(timeout: 10))
        XCTAssertEqual(draft.value as? String, "[success] Retain this project draft")
        app.buttons["task.send"].tap()
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitUntilExists(timeout: 25))
        let answerID = answer.identifier
        let number = try XCTUnwrap(answerID.split(separator: "-").dropLast().last)
        let row = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(number)")]
        let message = app.textFields["task.message"]

        message.tap(); message.typeText("Keep this conversation draft")
        for cycle in 0 ..< 2 {
            // End an interactive pop near the edge so the navigation is cancelled.
            let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            edge.press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.4)),
                       withVelocity: .slow, thenHoldForDuration: 0.5)
            XCTAssertTrue(message.waitUntilExists(timeout: 10))
            XCTAssertTrue(app.descendants(matching: .any)[answerID].exists)
            backToTaskList(app, swipe: cycle.isMultiple(of: 2))

            XCTAssertTrue(row.waitUntilExists(timeout: 15)); row.tap()
            XCTAssertTrue(app.descendants(matching: .any)[answerID].waitUntilExists(timeout: 15))
            assertComposerAtBottomWithKeyboardHidden(in: app)
            XCTAssertEqual(message.value as? String, "Keep this conversation draft")
            backToTaskList(app, swipe: !cycle.isMultiple(of: 2))

            let compose = app
                .buttons[cycle.isMultiple(of: 2) ? "tasks.new.project.simulator-project" : "tasks.new.chat"]
            XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
            XCTAssertTrue(message.waitUntilExists(timeout: 10))
            XCTAssertTrue(message.isHittable)
            message.tap(); message.typeText("New draft \(cycle)")
            backToTaskList(app, swipe: cycle.isMultiple(of: 2))
            compose.tap()
            XCTAssertTrue(message.waitUntilExists(timeout: 10))
            XCTAssertEqual(message.value as? String, "New draft \(cycle)")
            backToTaskList(app, swipe: !cycle.isMultiple(of: 2))
            row.tap()
            XCTAssertTrue(app.descendants(matching: .any)[answerID].waitUntilExists(timeout: 15))
            assertComposerAtBottomWithKeyboardHidden(in: app)
        }
        captureScreen(app, named: "Task reopened after repeated back and cancelled edge swipes")
    }

    private func assertUnavailableScannerCanBeCancelled(_ app: XCUIApplication) {
        app.buttons["pairing.scan"].tap()
        let scannerCancel = app.buttons["scanner.cancel"]
        XCTAssertTrue(scannerCancel.waitUntilExists(timeout: 10))
        allowFirstSystemPermissionIfPresent()
        let error = app.staticTexts["scanner.error"]
        XCTAssertTrue(error.waitUntilExists(timeout: 10))
        XCTAssertTrue(error.label.contains("カメラ"))
        scannerCancel.tap()
        XCTAssertTrue(app.buttons["pairing.scan"].waitUntilExists(timeout: 10))
        XCTAssertFalse(scannerCancel.exists)
    }

    private func backToTaskList(_ app: XCUIApplication, swipe: Bool) {
        if swipe {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
                .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        } else {
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 15))
        XCTAssertFalse(app.textFields["task.message"].exists)
    }
}
