import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorNativeTerminalRetainsShellAfterReopening() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        func openTerminal() {
            if app.buttons["task.more"].exists {
                app.buttons["task.more"].tap()
            }
            let action = app.buttons["task.terminal"]
            XCTAssertTrue(action.waitForExistence(timeout: 10)); action.tap()
            XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        }
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("Keep my draft")
        let chat = app.descendants(matching: .any)["task.empty"]
        XCTAssertTrue(chat.waitForExistence(timeout: 10))
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0))
            .withOffset(CGVector(dx: 0, dy: chat.frame.midY - app.frame.minY))
        edge.press(forDuration: 0.05, thenDragTo: edge.withOffset(CGVector(dx: -200, dy: 0)))
        XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitForExistence(timeout: 5))
        terminal.tap(); terminal.typeText("BEX_NATIVE=17\n")
        captureScreen(app, named: "Native terminal with keyboard")
        XCTAssertFalse(app.buttons["終了"].exists)
        XCTAssertFalse(app.buttons["terminal.close"].exists)
        app.buttons["workbench.files"].tap()
        let file = app.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitForExistence(timeout: 10))
        captureScreen(app, named: "Files in the chat right panel")
        file.tap()
        XCTAssertTrue(app.textViews["file.editor"].waitForExistence(timeout: 10))
        app.buttons["file.close"].tap()
        app.buttons["workbench.terminal"].tap()
        XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        XCTAssertGreaterThan(terminal.frame.minX, 0)
        terminal.swipeRight()
        XCTAssertTrue(app.buttons["task.terminal"].waitForExistence(timeout: 5))
        XCTAssertFalse(terminal.exists)
        XCTAssertEqual(composer.value as? String, "Keep my draft")
        openTerminal()
        terminal.tap(); terminal.typeText("exit $BEX_NATIVE\n")
        XCTAssertTrue(app.staticTexts["終了 · 17"].waitForExistence(timeout: 10))
        captureScreen(app, named: "Reattached native shell retains state")
        closeWorkbench(app)
    }

    func testPhysicalDeviceCanPairWithManualPayload() throws {
        guard let payload = ProcessInfo.processInfo.environment["BEX_PAIRING_PAYLOAD"],
              !payload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            throw XCTSkip("BEX_PAIRING_PAYLOAD is required for the physical-device pairing test")
        }

        let app = XCUIApplication()
        app.launch()
        allowFirstSystemPermissionIfPresent()

        submitManualPairing(app, payload: payload)

        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not appear after pairing")
        XCTAssertFalse(app.staticTexts["PCとペアリング"].exists)

        let notice = app.staticTexts["notice"]
        let project = prefixedButton(app, prefix: "tasks.project.")
        if project.exists, project.value as? String == "閉じています" {
            project.tap()
        }
        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(
            firstTask.waitForExistence(timeout: 30),
            "No task row appeared in the task list; notice: \(notice.exists ? notice.label : "(none)")"
        )
        firstTask.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Task detail content did not appear; notice: \(notice.exists ? notice.label : "(none)")"
        )

        let firstItem = assertLoadedTaskDetails(app)

        app.terminate()
        app.launch()

        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Relaunch did not open the task list")
        XCTAssertFalse(taskDetail.exists)
        if project.exists, project.value as? String == "閉じています" {
            project.tap()
        }
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitForExistence(timeout: 30))
        app.navigationBars.buttons.element(boundBy: 0).tap()

        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not reappear after returning")
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitForExistence(timeout: 30))
        captureScreen(app, named: "Native conversation navigation")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Native edge swipe did not return to the task list")
        XCTAssertFalse(taskDetail.exists)
    }

    func submitManualPairing(_ app: XCUIApplication, payload: String) {
        let manualPairing = app.buttons["QRの内容を手入力"]
        XCTAssertTrue(manualPairing.waitForExistence(timeout: 10))
        manualPairing.tap()

        let contents = app.secureTextFields["pairing.contents"]
        XCTAssertTrue(contents.waitForExistence(timeout: 10))
        contents.tap()
        contents.typeText(payload)
        XCTAssertFalse((contents.value as? String ?? "").isEmpty)

        let submit = app.buttons["pairing.submit"]
        XCTAssertTrue(submit.waitForExistence(timeout: 10))
        let submitEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: submit)
        wait(for: [submitEnabled], timeout: 10)
        submit.tap()
    }

    func assertLoadedTaskDetails(_ app: XCUIApplication) -> XCUIElement {
        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let loadingText = app.staticTexts["タスクを読み込み中…"]
        let loadingFinished = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: loadingText
        )
        let loadingResult = XCTWaiter.wait(for: [loadingFinished], timeout: 30)
        XCTAssertTrue(loadingResult == .completed, "Task detail remained on the loading screen")

        let detailMetrics = taskDetail.value as? String ?? "(unavailable)"
        let firstItem = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item."))
            .firstMatch
        XCTAssertTrue(
            firstItem.waitForExistence(timeout: 30),
            "No rendered task item appeared; detail=\(detailMetrics)"
        )

        let backButton = app.navigationBars.buttons.element(boundBy: 0)
        let composer = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(
            composer.exists && composer.isHittable,
            "Task composer was not visible and hittable after task detail loaded; detail=\(detailMetrics)"
        )
        XCTAssertTrue(
            backButton.exists && backButton.isHittable,
            "Task back button was not visible and hittable after task detail loaded; detail=\(detailMetrics)"
        )
        return firstItem
    }
}
