import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorRemovesHostAndRequiresPairingAfterRelaunch() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.hosts"].tap()
        let remove = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "connection.remove."))
            .firstMatch
        XCTAssertTrue(remove.waitForExistence(timeout: 10))
        captureScreen(app, named: "PC list connection removal")
        remove.tap()
        app.alerts.buttons["キャンセル"].tap()
        XCTAssertTrue(remove.exists)
        remove.tap()
        app.alerts.buttons["接続を解除"].tap()
        XCTAssertTrue(app.buttons["pairing.scan"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.descendants(matching: .any)["tasks.list"].exists)
        app.terminate(); app.launch()
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
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(threadNumber)"]
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
        XCTAssertTrue(host.waitForExistence(timeout: 10)); host.tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
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

    func testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Native project disclosure")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        openFiles(app)
        let nested = app.buttons["file.nested"]
        XCTAssertTrue(nested.waitForExistence(timeout: 10)); nested.tap()
        let child = app.buttons["file.child.txt"]
        XCTAssertTrue(child.waitForExistence(timeout: 10))
        captureScreen(app, named: "Native directory navigation")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(app.buttons["file.hello.txt"].waitForExistence(timeout: 10))
        XCTAssertFalse(child.exists)
        try verifyAbsoluteDirectoryNavigation(app, child: child)
        app.buttons["files.close"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    func testSimulatorUsesClaudeWithoutCodexAndRestoresConversation() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        XCTAssertTrue(app.buttons["model.settings"].waitForExistence(timeout: 10))
        app.buttons["model.settings"].tap()
        let choice = app.buttons["model.choice.claude:default"]
        XCTAssertTrue(choice.waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["model.account.desktop"].exists)
        choice.tap()
        captureScreen(app, named: "Claude models without Codex")
        app.buttons["model.close"].tap()
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
        expandSimulatorProject(app)
        let row = prefixedElement(app, prefix: "tasks.row.claude:")
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

    private func verifyAbsoluteDirectoryNavigation(_ app: XCUIApplication, child: XCUIElement) throws {
        let path = app.textFields["絶対パス"]
        let root = try XCTUnwrap(path.value as? String)
        XCTAssertTrue(root.hasPrefix("/"))
        path.tap(); path.press(forDuration: 1.2)
        let selectAll = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == 'Select All' OR label == 'すべてを選択'")).firstMatch
        XCTAssertTrue(selectAll.waitForExistence(timeout: 5)); selectAll.tap()
        path.typeText(root + "/nested")
        for _ in 0 ..< 2 {
            XCTAssertEqual(path.value as? String, root + "/nested")
            app.buttons["files.open-path"].tap()
            XCTAssertTrue(child.waitForExistence(timeout: 10))
            app.navigationBars["nested"].buttons.element(boundBy: 0).tap()
            XCTAssertTrue(app.buttons["file.hello.txt"].waitForExistence(timeout: 10))
            XCTAssertFalse(child.exists)
        }
    }

    func chooseFixtureModel(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 10)); choice.tap()
        app.segmentedControls["model.quick.effort"].buttons["high"].tap()
        app.buttons["model.close"].tap()
    }

    func verifyRestoredModelSettings(_ app: XCUIApplication, settings: XCUIElement) {
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["model.choice.fixture-model"].exists)
        XCTAssertGreaterThan(settings.frame.midX, app.frame.midX)
        XCTAssertGreaterThan(settings.frame.midY, app.descendants(matching: .any)["task.message"].frame.midY)
        settings.tap()
        let choice = app.buttons["model.choice.fixture-model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 10))
        XCTAssertEqual(choice.value as? String, "選択中")
        let efforts = app.segmentedControls["model.quick.effort"]
        XCTAssertTrue(efforts.buttons["high"].isSelected)
        efforts.buttons["medium"].tap()
        XCTAssertTrue(efforts.buttons["medium"].isSelected)
        efforts.buttons["high"].tap()
        captureScreen(app, named: "Account and model settings")
        app.buttons["model.close"].tap()
    }

    func testSimulatorSwitchesCodexAccountsAndForksConversation() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Inherit this question")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let originalAnswerID = answer.identifier
        let firstFork = app.buttons["response.fork." + String(originalAnswerID.dropFirst("item.".count))]
        XCTAssertTrue(firstFork.waitForExistence(timeout: 10))
        switchFixtureAccount(app)
        XCTAssertTrue(app.descendants(matching: .any)[originalAnswerID].exists)
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("[success] This later question stays in the original")
        app.buttons["task.send"].tap()
        let nextID = String(originalAnswerID.dropLast()) + "2"
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitForExistence(timeout: 25))
        // Fork the earlier completed turn after the original has a later turn.
        let list = app.descendants(matching: .any)["task.detail"]
        for _ in 0 ..< 6 where !firstFork.isHittable {
            list.swipeDown()
        }
        XCTAssertTrue(firstFork.isHittable); firstFork.tap()
        let disappeared = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: app.descendants(matching: .any)[nextID]
        )
        wait(for: [disappeared], timeout: 20)
        XCTAssertTrue(app.descendants(matching: .any)[originalAnswerID].waitForExistence(timeout: 20))
        composer.tap(); composer.typeText("[success] Continue the fork")
        app.buttons["task.send"].tap()
        let forkAnswer = app.descendants(matching: .any).matching(NSPredicate(
            format: "identifier BEGINSWITH 'item.fixture-final-' AND identifier != %@",
            originalAnswerID
        )).firstMatch
        XCTAssertTrue(forkAnswer.waitForExistence(timeout: 25))
        XCTAssertNotEqual(forkAnswer.identifier, nextID)
        captureScreen(app, named: "Fork inherits the selected answer and accepts a new turn")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let number = try XCTUnwrap(originalAnswerID.split(separator: "-").dropLast().last)
        let original = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(original.waitForExistence(timeout: 15)); original.tap()
        XCTAssertTrue(app.descendants(matching: .any)[nextID].waitForExistence(timeout: 20))
    }

    private func switchFixtureAccount(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        let desktop = app.buttons["model.account.desktop"]
        XCTAssertTrue(desktop.waitForExistence(timeout: 10))
        XCTAssertEqual(desktop.value as? String, "選択中")
        app.buttons["model.account.add"].tap()
        XCTAssertTrue(app.staticTexts["model.login.code"].waitForExistence(timeout: 10))
        XCUIDevice.shared.press(.home)
        app.activate()
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'model.account.account-'"))
            .firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 20))
        let selected = expectation(for: NSPredicate(format: "value == %@", "選択中"), evaluatedWith: second)
        wait(for: [selected], timeout: 15)
        app.buttons["model.choice.fixture-model"].tap()
        app.segmentedControls["model.quick.effort"].buttons["high"].tap()
        captureScreen(app, named: "Two Codex accounts sharing conversation history")
        app.buttons["model.close"].tap()
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
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
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
