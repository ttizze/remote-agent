import XCTest

final class BexLaunchUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func testLaunchKeepsPairingScreenAlive() {
        let app = XCUIApplication()
        app.launch()

        let pairingTitle = app.staticTexts["PCとペアリング"]
        XCTAssertTrue(pairingTitle.waitForExistence(timeout: 10))

        let stayedAlive = XCTWaiter.wait(
            for: [XCTestExpectation(description: "observe process stability")],
            timeout: 2
        )
        XCTAssertEqual(stayedAlive, .timedOut)
        XCTAssertEqual(app.state, .runningForeground)
        XCTAssertTrue(pairingTitle.exists)
    }

    func testPhysicalDeviceCanPairWithManualPayload() throws {
        guard let payload = ProcessInfo.processInfo.environment["BEX_PAIRING_PAYLOAD"],
              !payload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw XCTSkip("BEX_PAIRING_PAYLOAD is required for the physical-device pairing test")
        }

        let app = XCUIApplication()
        app.launch()
        allowFirstSystemPermissionIfPresent()

        let manualPairing = app.buttons["QRの内容を手入力"]
        XCTAssertTrue(manualPairing.waitForExistence(timeout: 10))
        manualPairing.tap()

        let contents = app.textViews["pairing.contents"]
        XCTAssertTrue(contents.waitForExistence(timeout: 10))
        contents.tap()
        contents.typeText(payload)
        XCTAssertEqual(contents.value as? String, payload, "Pairing payload was not entered verbatim")

        let submit = app.buttons["pairing.submit"]
        XCTAssertTrue(submit.waitForExistence(timeout: 10))
        let submitEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: submit)
        wait(for: [submitEnabled], timeout: 10)
        submit.tap()

        let connectButton = app.buttons["connect.start"]
        let paired = connectButton.waitForExistence(timeout: 30)
        let pairingNotice = app.staticTexts["notice"]
        let pairingFailure = pairingNotice.exists ? pairingNotice.label : "(none)"
        XCTAssertTrue(
            paired,
            "Pairing did not complete; notice: \(pairingFailure)",
        )
        XCTAssertFalse(app.staticTexts["PCとペアリング"].exists)

        connectButton.tap()

        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not appear after connecting")

        let notice = app.staticTexts["notice"]
        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(
            firstTask.waitForExistence(timeout: 30),
            "No task row appeared in the task list; notice: \(notice.exists ? notice.label : "(none)")",
        )
        firstTask.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Task detail content did not appear; notice: \(notice.exists ? notice.label : "(none)")",
        )

        let loadingText = app.staticTexts["タスクを読み込み中…"]
        let loadingFinished = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: loadingText,
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
            "No rendered task item appeared; detail=\(detailMetrics)",
        )

        let backButton = app.buttons["task.back"]
        let composer = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(
            composer.exists && composer.isHittable,
            "Task composer was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )
        XCTAssertTrue(
            backButton.exists && backButton.isHittable,
            "Task back button was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )

        app.terminate()
        app.launch()

        let reconnectButton = app.buttons["connect.start"]
        let reconnectTaskList = app.descendants(matching: .any)["tasks.list"]
        let reconnectTaskDetail = app.descendants(matching: .any)["task.detail"]
        let reconnectReady = expectation(
            for: NSPredicate { _, _ in reconnectTaskList.exists || reconnectButton.exists },
            evaluatedWith: app,
        )
        let reconnectResult = XCTWaiter.wait(for: [reconnectReady], timeout: 30)
        XCTAssertTrue(
            reconnectResult == .completed,
            "Neither task list nor connect button appeared after relaunch; task.detail.exists=\(reconnectTaskDetail.exists); notice: \(notice.exists ? notice.label : "(none)")",
        )
        if !reconnectTaskList.exists {
            reconnectButton.tap()
        }
        XCTAssertTrue(
            reconnectTaskList.waitForExistence(timeout: 30),
            "Task list did not appear after reconnect; task.detail.exists=\(reconnectTaskDetail.exists); notice: \(notice.exists ? notice.label : "(none)")",
        )
        XCTAssertFalse(reconnectTaskDetail.exists, "Reconnect restored task detail instead of the task list")
    }

    func testOpeningTaskAndReturningShowsTaskList() {
        let app = XCUIApplication()
        app.terminate()
        app.launch()

        let connectButton = app.buttons["connect.start"]
        let taskList = app.descendants(matching: .any)["tasks.list"]
        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let connectionNotice = app.staticTexts["notice"]
        let connectionReady = expectation(
            for: NSPredicate { _, _ in taskList.exists || connectButton.exists },
            evaluatedWith: app,
        )
        let connectionResult = XCTWaiter.wait(for: [connectionReady], timeout: 30)
        XCTAssertTrue(
            connectionResult == .completed,
            "Neither task list nor connect button appeared after launch; task.detail.exists=\(taskDetail.exists); notice: \(connectionNotice.exists ? connectionNotice.label : "(none)")",
        )
        if !taskList.exists {
            connectButton.tap()
        }
        XCTAssertTrue(
            taskList.waitForExistence(timeout: 30),
            "Task list did not appear after launch/connect; task.detail.exists=\(taskDetail.exists); notice: \(connectionNotice.exists ? connectionNotice.label : "(none)")",
        )
        XCTAssertFalse(taskDetail.exists, "Launch/connect restored task detail instead of the task list")

        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30), "No task row appeared in the task list")
        firstTask.tap()

        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Task detail content did not appear; notice: \(connectionNotice.exists ? connectionNotice.label : "(none)")",
        )

        let loadingText = app.staticTexts["タスクを読み込み中…"]
        let loadingFinished = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: loadingText,
        )
        let loadingResult = XCTWaiter.wait(for: [loadingFinished], timeout: 30)
        XCTAssertEqual(
            loadingResult,
            .completed,
            "Task detail remained on the loading screen",
        )

        let detailMetrics = taskDetail.value as? String ?? "(unavailable)"
        let firstItem = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item."))
            .firstMatch
        XCTAssertTrue(
            firstItem.waitForExistence(timeout: 30),
            "No rendered task item appeared; detail=\(detailMetrics)",
        )

        let backButton = app.buttons["task.back"]
        let composer = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(
            composer.exists && composer.isHittable,
            "Task composer was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )
        XCTAssertTrue(
            backButton.exists && backButton.isHittable,
            "Task back button was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )
        backButton.tap()

        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not reappear after returning")
    }

    private func allowFirstSystemPermissionIfPresent() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let alert = springboard.alerts.firstMatch
        guard alert.waitForExistence(timeout: 3) else { return }

        let positiveLabels = [
            "許可",
            "Allow",
            "Allow While Using App",
            "Allow Once",
            "許可する",
            "一度だけ許可",
        ]
        for label in positiveLabels {
            let button = alert.buttons[label]
            if button.exists {
                button.tap()
                return
            }
        }
    }
}
