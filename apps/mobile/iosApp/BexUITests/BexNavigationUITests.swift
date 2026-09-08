import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
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

    func testSimulatorReturnsToListWithNativeEdgeSwipeAndRetainsDrafts() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let message = app.textFields["task.message"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        message.tap(); message.typeText("[success] Retain this project draft")
        func swipeBack() {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
                .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
            XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
            XCTAssertFalse(message.exists)
        }
        swipeBack()
        compose.tap()
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertEqual(message.value as? String, "[success] Retain this project draft")
        app.buttons["task.send"].tap()
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let answerID = answer.identifier
        let number = try XCTUnwrap(answerID.split(separator: "-").dropLast().last)
        message.tap(); message.typeText("Keep this conversation draft")
        swipeBack()
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 15))
        XCTAssertEqual(message.value as? String, "Keep this conversation draft")
        captureScreen(app, named: "Native navigation after edge swipe with retained draft")
        swipeBack()
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
        let number = try XCTUnwrap(answer.identifier.split(separator: "-").dropLast().last)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.waitForExistence(timeout: 10)); project.tap()
        XCTAssertFalse(row.exists)
        project.tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
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
        app.buttons["files.close"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    func chooseFixtureModel(_ app: XCUIApplication) {
        app.buttons["model.settings"].tap()
        XCTAssertFalse(app.buttons["model.picker"].exists)
        app.buttons["model.details"].tap()
        let model = app.buttons["model.picker"]
        XCTAssertTrue(model.waitForExistence(timeout: 10)); model.tap()
        let choice = app.buttons["Fixture Model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 10)); choice.tap()
        let effort = app.buttons["model.effort"]
        XCTAssertTrue(effort.waitForExistence(timeout: 10)); effort.tap()
        app.buttons["high"].tap()
        app.buttons["model.close"].tap()
    }

    func verifyRestoredModelSettings(_ app: XCUIApplication, settings: XCUIElement) {
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["model.picker"].exists)
        XCTAssertGreaterThan(settings.frame.midX, app.frame.midX)
        XCTAssertGreaterThan(settings.frame.midY, app.descendants(matching: .any)["task.message"].frame.midY)
        settings.tap()
        XCTAssertFalse(app.buttons["model.picker"].exists)
        XCTAssertTrue(app.buttons["model.details"].label.contains("Fixture Model"))
        app.segmentedControls["model.quick.effort"].buttons["medium"].tap()
        XCTAssertTrue(app.buttons["model.details"].label.contains("medium"))
        app.segmentedControls["model.quick.effort"].buttons["high"].tap()
        captureScreen(app, named: "Quick model settings")
        app.buttons["model.details"].tap()
        XCTAssertTrue(app.buttons["model.picker"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["model.picker"].label.contains("Fixture Model"))
        XCTAssertTrue(app.buttons["model.effort"].label.contains("high"))
        app.buttons["model.effort"].tap(); app.buttons["medium"].tap()
        app.buttons["model.effort"].tap(); app.buttons["high"].tap()
        app.buttons["model.close"].tap()
    }

    func testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Repeated navigation fixture")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let answerID = answer.identifier
        let number = try XCTUnwrap(answerID.split(separator: "-").dropLast().last)
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        let message = app.textFields["task.message"]
        let list = app.descendants(matching: .any)["tasks.list"]

        func back(swipe: Bool) {
            if swipe {
                app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
                    .press(
                        forDuration: 0.1,
                        thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4))
                    )
            } else {
                app.navigationBars.buttons.element(boundBy: 0).tap()
            }
            XCTAssertTrue(list.waitForExistence(timeout: 15))
            XCTAssertFalse(message.exists)
        }

        for cycle in 0 ..< 6 {
            // End an interactive pop near the edge so the navigation is cancelled.
            let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            edge.press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.4)),
                       withVelocity: .slow, thenHoldForDuration: 0.5)
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            XCTAssertTrue(app.descendants(matching: .any)[answerID].exists)
            back(swipe: cycle.isMultiple(of: 2))

            XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
            XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 15))
            back(swipe: !cycle.isMultiple(of: 2))

            let compose = app
                .buttons[cycle.isMultiple(of: 2) ? "tasks.new.project.simulator-project" : "tasks.new.chat"]
            XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            XCTAssertTrue(message.isHittable)
            back(swipe: cycle.isMultiple(of: 2))
            row.tap()
            XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 15))
        }
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Task reopened after repeated back and cancelled edge swipes"
        screenshot.lifetime = .keepAlways; add(screenshot)
    }
}
