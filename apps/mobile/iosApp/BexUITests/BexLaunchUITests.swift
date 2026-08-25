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

        let submit = app.buttons["pairing.submit"]
        XCTAssertTrue(submit.waitForExistence(timeout: 10))
        let submitEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: submit)
        wait(for: [submitEnabled], timeout: 10)
        submit.tap()

        let connectButton = app.buttons["connect.start"]
        XCTAssertTrue(connectButton.waitForExistence(timeout: 30))
        XCTAssertFalse(app.staticTexts["PCとペアリング"].exists)

        connectButton.tap()

        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not appear after connecting")

        let taskOutcome = app
            .descendants(matching: .any)
            .matching(NSPredicate(
                format: "identifier == %@ OR identifier BEGINSWITH %@",
                "tasks.empty",
                "tasks.row.",
            ))
            .firstMatch
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskOutcome.waitForExistence(timeout: 30),
            "Neither an existing task row nor the empty state appeared; notice: \(notice.exists ? notice.label : "(none)")",
        )
    }

    func testOpeningTaskAndReturningShowsTaskList() {
        let app = XCUIApplication()
        app.terminate()
        app.launch()

        let connectButton = app.buttons["connect.start"]
        XCTAssertTrue(connectButton.waitForExistence(timeout: 30), "Connect button did not appear")
        connectButton.tap()

        let taskList = app.descendants(matching: .any)["tasks.list"]
        let backButton = app.buttons["task.back"]
        if !taskList.waitForExistence(timeout: 3) {
            XCTAssertTrue(
                backButton.waitForExistence(timeout: 30),
                "Neither the task list nor the persisted task detail appeared after connecting",
            )
            backButton.tap()
        }
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not appear after connecting")

        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30), "No task row appeared in the task list")
        firstTask.tap()

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

        XCTAssertTrue(backButton.waitForExistence(timeout: 30), "Task back button did not appear")
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
