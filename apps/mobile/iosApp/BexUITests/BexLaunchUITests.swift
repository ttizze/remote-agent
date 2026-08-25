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

    func testSimulatorCanStartAConversationInAProject() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This isolated conversation-start E2E runs only in the iOS Simulator")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Start the simulator conversation")

        let activity = prefixedElement(app, prefix: "turn.activity.fixture-turn-")
        let streamedCommand = prefixedElement(app, prefix: "item.fixture-command-")
        XCTAssertTrue(activity.waitForExistence(timeout: 10), "Streaming activity header did not appear")
        XCTAssertTrue(streamedCommand.waitForExistence(timeout: 10), "Streaming command was not rendered expanded")
        XCTAssertFalse(
            prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists,
            "In-progress work must not be collapsible",
        )

        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 15), "Final answer did not stream into the conversation")
        let commandCollapsed = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: streamedCommand,
        )
        wait(for: [commandCollapsed], timeout: 10)
        XCTAssertTrue(
            prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists,
            "Completed work did not become an expandable collapsed summary",
        )
    }

    func testSimulatorShowsRetryingStreamErrorThenRecovers() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only conversation display E2E")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[retry] Retry stream")

        let retrying = app.staticTexts["サーバーが混み合っています。再接続しています"]
        XCTAssertTrue(retrying.waitForExistence(timeout: 10), "Retrying stream error was not visible")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))
        let recovered = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: retrying)
        wait(for: [recovered], timeout: 10)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
    }

    func testSimulatorKeepsFailedWorkExpandedWithTerminalError() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only conversation display E2E")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[failed] Fail turn")

        XCTAssertTrue(app.staticTexts["コンテキストの上限に達しました"].waitForExistence(timeout: 15))
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-final-").exists)
    }

    func testSimulatorKeepsInterruptedWorkExpanded() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only conversation display E2E")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[interrupted] Interrupt turn")

        XCTAssertTrue(app.staticTexts["3s間作業した後に中断しました"].waitForExistence(timeout: 15))
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
    }

    func testSimulatorKeepsInputRequestVisibleUntilResolved() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only conversation display E2E")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[request] Ask user")

        let request = prefixedElement(app, prefix: "request.host-proxy-")
        XCTAssertTrue(request.waitForExistence(timeout: 10), "Pending request was not visible")
        XCTAssertTrue(app.staticTexts["回答待ち"].exists)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))
        let resolved = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: request)
        wait(for: [resolved], timeout: 10)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
    }

    func testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only conversation display E2E")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[items] Render item families")

        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 20))
        let activity = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(activity.waitForExistence(timeout: 10))
        activity.tap()

        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertEqual(detail.value as? String, "turns=1;items=13")
        for _ in 0..<10 { detail.swipeDown() }

        for prefix in [
            "item.fixture-plan-", "item.fixture-mcp-", "item.fixture-dynamic-",
            "item.fixture-collab-", "item.fixture-subagent-", "item.fixture-web-",
            "item.fixture-image-", "item.fixture-compaction-",
        ] {
            XCTAssertTrue(waitForPrefixedElement(app, prefix: prefix, scrolling: detail), prefix)
        }
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-sleep-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-review-in-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-review-out-").exists)
    }

    func testSimulatorReopensCompletedHistoryCollapsed() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only conversation display E2E")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[history] Reopen history")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)

        app.buttons["task.back"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 10))
        let newestTask = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(newestTask.waitForExistence(timeout: 10))
        newestTask.tap()

        XCTAssertTrue(app.descendants(matching: .any)["task.detail"].waitForExistence(timeout: 15))
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
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

    private func connectedSimulatorApp() throws -> XCUIApplication {
        let app = XCUIApplication()
        app.launch()
        allowFirstSystemPermissionIfPresent()

        let taskList = app.descendants(matching: .any)["tasks.list"]
        if taskList.waitForExistence(timeout: 3) {
            return app
        }

        let manualPairing = app.buttons["QRの内容を手入力"]
        if manualPairing.waitForExistence(timeout: 3) {
            let payload = try simulatorPairingPayload()
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
        }

        let connectButton = app.buttons["connect.start"]
        if connectButton.waitForExistence(timeout: 10) {
            connectButton.tap()
        }
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskList.waitForExistence(timeout: 30),
            "Task list did not appear after connecting; notice: \(notice.exists ? notice.label : "(none)")"
        )
        return app
    }

    private func startSimulatorConversation(_ app: XCUIApplication, promptText: String) throws {
        let projectCompose = prefixedElement(app, prefix: "tasks.new.project.")
        XCTAssertTrue(projectCompose.waitForExistence(timeout: 10), "No project creation button appeared")
        projectCompose.tap()

        let prompt = app.textViews["new-task.prompt"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap()
        prompt.typeText(promptText)

        let start = app.buttons["new-task.start"]
        XCTAssertTrue(start.waitForExistence(timeout: 10))
        let startEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: start)
        wait(for: [startEnabled], timeout: 10)
        start.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Conversation did not open; notice: \(notice.exists ? notice.label : "(none)")",
        )
        XCTAssertTrue(app.descendants(matching: .any)["task.message"].isHittable)
    }

    private func prefixedElement(_ app: XCUIApplication, prefix: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
            .firstMatch
    }

    private func prefixedButton(_ app: XCUIApplication, prefix: String) -> XCUIElement {
        app.buttons
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
            .firstMatch
    }

    private func waitForPrefixedElement(
        _ app: XCUIApplication,
        prefix: String,
        scrolling scrollView: XCUIElement
    ) -> Bool {
        let element = prefixedElement(app, prefix: prefix)
        if element.waitForExistence(timeout: 1) { return true }
        for _ in 0..<14 {
            scrollView.swipeUp()
            if element.waitForExistence(timeout: 1) { return true }
        }
        return false
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

    private func simulatorPairingPayload() throws -> String {
        let url = try XCTUnwrap(URL(string: "http://127.0.0.1:50438/pairing"))
        let data = try Data(contentsOf: url)
        let payload = try XCTUnwrap(String(data: data, encoding: .utf8))
        guard !payload.isEmpty else { throw PairingPayloadError.invalidResponse }
        return payload
    }
}

private enum PairingPayloadError: Error {
    case invalidResponse
}
