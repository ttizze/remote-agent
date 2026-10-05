import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorRemovesHostAndRequiresPairingAfterRelaunch() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.hosts"].tap()
        let profile = prefixedButton(app, prefix: "profiles.")
        XCTAssertTrue(profile.waitForExistence(timeout: 10))
        captureScreen(app, named: "Redesigned saved PC screen")
        profile.press(forDuration: 1)
        let remove = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "connection.remove."))
            .firstMatch
        XCTAssertTrue(remove.waitForExistence(timeout: 10))
        captureScreen(app, named: "PC list connection removal")
        remove.tap()
        app.alerts.buttons["キャンセル"].tap()
        profile.press(forDuration: 1)
        XCTAssertTrue(remove.waitForExistence(timeout: 5))
        remove.tap()
        app.alerts.buttons["接続を解除"].tap()
        XCTAssertTrue(app.staticTexts["pairing.welcome"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["pairing.scan"].exists)
        XCTAssertEqual(app.buttons["pairing.scan"].label, "QRコードを読み取る")
        XCTAssertFalse(app.descendants(matching: .any)["tasks.list"].exists)
        app.terminate(); app.launch()
        XCTAssertTrue(app.staticTexts["pairing.welcome"].waitForExistence(timeout: 10))
        app.buttons["privacy.policy"].tap()
        XCTAssertTrue(app.navigationBars["プライバシーポリシー"].waitForExistence(timeout: 5))
        app.buttons["閉じる"].tap()
        XCTAssertTrue(app.buttons["pairing.scan"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.descendants(matching: .any)["tasks.list"].exists)
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
        XCTAssertTrue(projectCompose.waitForExistence(timeout: 10)); projectCompose.tap()
        chooseFixtureModel(app)
        let prompt = app.descendants(matching: .any)["task.message"]
        prompt.tap(); prompt.typeText("[model] Verify chosen settings")
        app.buttons["task.send"].tap()
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 25), "Chosen settings were rejected by the fixture")
        let finalID = finalAnswer.identifier
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.exists)
        app.terminate(); app.launch()
        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30))
        XCTAssertFalse(detail.exists)
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.waitForExistence(timeout: 10))
        XCTAssertEqual(project.value as? String, "閉じています")
        project.tap()
        let threadNumber = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-thread-\(threadNumber)")]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitForExistence(timeout: 30))
        let settings = app.buttons["model.settings"]
        verifyRestoredModelSettings(app, settings: settings)
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(detail.waitForExistence(timeout: 15))
        captureScreen(app, named: "Open conversation retained on foreground")
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitForExistence(timeout: 20))
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10)); composer.tap()
        composer.typeText("[model] Verify settings after restart")
        let send = app.buttons["task.send"]
        let ready = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        wait(for: [ready], timeout: 30); send.tap()
        let nextAnswer = app.descendants(matching: .any)[String(finalID.dropLast()) + "2"]
        XCTAssertTrue(nextAnswer.waitForExistence(timeout: 25))
        composer.tap()
        XCTAssertTrue(settings.isHittable)
        captureScreen(app, named: "Restored task and selected model")
    }

    func testSimulatorUsesNativeHostNavigationAndPairingDismissal() throws {
        let app = try connectedSimulatorApp()
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(
            format: "label BEGINSWITH %@", "検証 Host、"
        )).firstMatch.waitForExistence(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let addHost = app.buttons["profiles.add"]
        XCTAssertTrue(addHost.waitForExistence(timeout: 10)); addHost.tap()
        let cancel = app.buttons["pairing.cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.13))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)))
        XCTAssertTrue(addHost.waitForExistence(timeout: 10))
        XCTAssertFalse(cancel.exists)
        addHost.tap()
        XCTAssertTrue(cancel.waitForExistence(timeout: 10)); cancel.tap()
        let host = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH %@ AND identifier != %@",
            "profiles.",
            "profiles.add"
        )).firstMatch
        XCTAssertTrue(host.waitForExistence(timeout: 10))
        XCTAssertTrue(host.label.contains("検証 Host"))
        host.tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))

        app.buttons["tasks.hosts"].tap()
        addHost.tap()
        app.buttons["pairing.manual"].tap()
        let contents = app.secureTextFields["pairing.contents"]
        contents.tap(); contents.typeText("invalid invitation")
        app.buttons["pairing.submit"].tap()
        XCTAssertTrue(app.staticTexts["notice"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.keyboards.firstMatch.exists)
        contents.tap()
        let payload = try simulatorPairingPayload()
        contents.typeText(payload)
        app.buttons["pairing.submit"].tap()
        XCTAssertTrue(app.buttons["pairing.change"].waitForExistence(timeout: 10))
        app.buttons["pairing.change"].tap()
        submitManualPairing(app, payload: payload)
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 30))
        XCTAssertFalse(app.buttons["pairing.cancel"].exists)
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(
            format: "label BEGINSWITH %@", "検証 Host、"
        )).firstMatch.waitForExistence(timeout: 10))
        app.terminate(); app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 30))
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(
            format: "label BEGINSWITH %@", "検証 Host、"
        )).firstMatch.waitForExistence(timeout: 10))
    }

    func testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        XCTAssertGreaterThan(search.frame.midY, app.frame.height * 0.8)
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.exists)
        XCTAssertEqual(project.value as? String, "閉じています")
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.isHittable)
        captureScreen(app, named: "Remote projects with bottom search")
        compose.tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["tasks.new.chat"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        search.tap(); search.typeText("no-matching-conversation-unique\n")
        XCTAssertFalse(app.buttons["tasks.project.simulator-project"].exists)
        search.buttons.firstMatch.tap()
        app.buttons["close"].tap()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(project.waitForExistence(timeout: 10))
        project.tap()
        captureScreen(app, named: "Expanded project rows")
        XCTAssertEqual(project.value as? String, "開いています")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertEqual(project.value as? String, "開いています")
        compose.tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertEqual(project.value as? String, "開いています")
        app.buttons["tasks.new.chat"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    func testSimulatorUsesClaudeWithoutCodexAndRestoresConversation() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        app.buttons["tasks.new.chat"].tap()
        XCTAssertTrue(app.buttons["model.settings"].waitForExistence(timeout: 10))
        app.buttons["model.settings"].tap()
        _ = addFixtureClaudeAccount(app)
        openModelChoices(app)
        let choice = app.buttons["model.choice.default"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        choice.tap()
        captureScreen(app, named: "Claude models without Codex")
        dismissModelSettings(app)
        let message = app.textFields["task.message"]
        message.tap()
        message.typeText("Claude standalone")
        app.buttons["task.send"].tap()
        let answer = app.textViews["reply 1: Claude standalone"]
        XCTAssertTrue(answer.waitForExistence(timeout: 30))
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        app.terminate()
        app.launch()
        let row = prefixedElement(app, prefix: "tasks.row.")
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        XCTAssertTrue(answer.waitForExistence(timeout: 20))
        message.tap()
        message.typeText("Follow up")
        app.buttons["task.send"].tap()
        XCTAssertTrue(app.textViews["reply 2: Follow up"].waitForExistence(timeout: 30))
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Claude conversation restored and continued")
    }

    func openModelChoices(_ app: XCUIApplication) {
        if app.buttons["model.back"].exists {
            app.buttons["model.back"].tap()
        }
        XCTAssertTrue(app.textFields["model.search"].waitForExistence(timeout: 15))
    }

    func dismissModelSettings(_ app: XCUIApplication) {
        if app.buttons["model.close"].exists {
            app.buttons["model.close"].tap()
        } else if app.buttons["settings.scope.projects"].exists {
            let back = app.navigationBars.buttons["BackButton"]
            XCTAssertTrue(back.waitForExistence(timeout: 10))
            back.tap()
            let close = app.buttons["settings.close"]
            XCTAssertTrue(close.waitForExistence(timeout: 10))
            close.tap()
        } else {
            let picker = app.descendants(matching: .any)["model.picker"]
            XCTAssertTrue(picker.waitForExistence(timeout: 10))
            let handle = picker.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0))
                .withOffset(CGVector(dx: 0, dy: -12))
            handle.press(
                forDuration: 0.1,
                thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.98))
            )
        }
        let dismissed = expectation(for: NSPredicate(format: "exists == false"),
                                    evaluatedWith: app.textFields["model.search"])
        wait(for: [dismissed], timeout: 10)
    }

    func chooseFixtureEffort(_ app: XCUIApplication, _ value: String) {
        let control = app.buttons["model.effort"]
        XCTAssertTrue(control.waitForExistence(timeout: 10)); control.tap()
        app.buttons["model.effort." + value].tap()
    }

    func chooseFixtureModel(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        openModelChoices(app)
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 10)); choice.tap()
        dismissModelSettings(app)
        chooseFixtureEffort(app, "high")
    }

    func verifyRestoredModelSettings(_ app: XCUIApplication, settings: XCUIElement) {
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertGreaterThan(settings.frame.midY, app.descendants(matching: .any)["task.message"].frame.midY)
        settings.tap()
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 10))
        XCTAssertEqual(choice.value as? String, "選択中")
        captureScreen(app, named: "Restored model and account picker")
        dismissModelSettings(app)
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
        XCTAssertTrue(projectCompose.waitForExistence(timeout: 10)); projectCompose.tap()
        let draft = app.textFields["task.message"]
        XCTAssertTrue(draft.waitForExistence(timeout: 10))
        draft.tap(); draft.typeText("[success] Retain this project draft")
        backToTaskList(app, swipe: true)
        projectCompose.tap()
        XCTAssertTrue(draft.waitForExistence(timeout: 10))
        XCTAssertEqual(draft.value as? String, "[success] Retain this project draft")
        app.buttons["task.send"].tap()
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let answerID = answer.identifier
        let number = try XCTUnwrap(answerID.split(separator: "-").dropLast().last)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-thread-\(number)")]
        let message = app.textFields["task.message"]

        message.tap(); message.typeText("Keep this conversation draft")
        for cycle in 0 ..< 2 {
            // End an interactive pop near the edge so the navigation is cancelled.
            let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            edge.press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.4)),
                       withVelocity: .slow, thenHoldForDuration: 0.5)
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            XCTAssertTrue(app.descendants(matching: .any)[answerID].exists)
            backToTaskList(app, swipe: cycle.isMultiple(of: 2))

            XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
            XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 15))
            XCTAssertEqual(message.value as? String, "Keep this conversation draft")
            backToTaskList(app, swipe: !cycle.isMultiple(of: 2))

            let compose = app
                .buttons[cycle.isMultiple(of: 2) ? "tasks.new.project.simulator-project" : "tasks.new.chat"]
            XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            XCTAssertTrue(message.isHittable)
            message.tap(); message.typeText("New draft \(cycle)")
            backToTaskList(app, swipe: cycle.isMultiple(of: 2))
            compose.tap()
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            XCTAssertEqual(message.value as? String, "New draft \(cycle)")
            backToTaskList(app, swipe: !cycle.isMultiple(of: 2))
            row.tap()
            XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 15))
        }
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Task reopened after repeated back and cancelled edge swipes"
        screenshot.lifetime = .keepAlways; add(screenshot)
    }

    private func backToTaskList(_ app: XCUIApplication, swipe: Bool) {
        if swipe {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
                .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        } else {
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.textFields["task.message"].exists)
    }
}
