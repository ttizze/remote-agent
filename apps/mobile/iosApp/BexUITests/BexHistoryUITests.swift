import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorOpensLongInterruptedHistoryAtLatestMessage() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only isolated long-history fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-long-history")]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitForExistence(timeout: 30))
        let latest = app.descendants(matching: .any)["item.long-9-0"]
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: latest)
        wait(for: [visible], timeout: 5)
        captureScreen(app, named: "Long interrupted history at latest message")
        let initialItems = loadedItems(in: detail)
        XCTAssertGreaterThan(initialItems, 0)
        for _ in 0 ..< 40 {
            if loadedItems(in: detail) > initialItems {
                break
            }
            detail.swipeDown(velocity: .fast)
        }
        XCTAssertGreaterThan(loadedItems(in: detail), initialItems,
                             "Scrolling upward must load older items without tapping a button")
        XCTAssertFalse(latest.isHittable, "Prepending history must not jump back to the latest message")
        captureScreen(app, named: "Older history loaded by scrolling")
        assertHistoryTopNavigation(app, latest: latest)
        XCTAssertTrue(latest.isHittable)
        for _ in 0 ..< 4 {
            app.navigationBars.buttons.element(boundBy: 0).tap()
            XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
            XCTAssertTrue(latest.waitForExistence(timeout: 20))
            XCTAssertTrue(latest.isHittable, "Every reopen must render the latest message in the viewport")
        }
    }

    private func assertHistoryTopNavigation(_ app: XCUIApplication, latest: XCUIElement) {
        let latestButton = app.buttons["task.latest"]
        XCTAssertTrue(latestButton.waitForExistence(timeout: 5))
        latestButton.tap()
        for scrollToTop in [
            { app.buttons["task.top"].tap() },
            { app.coordinate(withNormalizedOffset: CGVector(dx: 0.18, dy: 0))
                .withOffset(CGVector(dx: 0, dy: 32)).tap() }
        ] {
            XCTAssertTrue(latest.isHittable)
            XCTAssertFalse(latestButton.exists)
            scrollToTop()
            XCTAssertTrue(latestButton.waitForExistence(timeout: 5))
            XCTAssertEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "history.")).count, 0)
            XCTAssertFalse(latest.isHittable, "Top navigation must not snap back to the latest message")
            latestButton.tap()
        }
    }

    func testSimulatorFillsInitialHistoryViewportWithoutScrolling() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("viewport-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-viewport-history")]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitForExistence(timeout: 30))
        let latest = app.descendants(matching: .any)["item.long-latest-message"]
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        let filled = expectation(for: NSPredicate { _, _ in self.loadedItems(in: detail) > 1 }, evaluatedWith: detail)
        wait(for: [filled], timeout: 20)
        XCTAssertTrue(latest.isHittable, "Automatic initial paging must keep the latest message visible")
        XCTAssertEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "history.")).count, 0)
        captureScreen(app, named: "Initial history fills the viewport without a loading button")
    }

    func testSimulatorReopensRunningLongHistoryWithoutBlankViewport() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-long-history")]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let latest = app.descendants(matching: .any)["item.long-9-0"]
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("[approval] Reopen while this turn is running")
        app.buttons["task.send"].tap()
        let running = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(running.waitForExistence(timeout: 10))
        let approval = app.buttons["request.accept"]
        XCTAssertTrue(approval.waitForExistence(timeout: 10))
        for _ in 0 ..< 4 {
            app.navigationBars.buttons.element(boundBy: 0).tap()
            XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
            XCTAssertTrue(running.waitForExistence(timeout: 10))
            XCTAssertTrue(approval.waitForExistence(timeout: 10))
            XCTAssertTrue(approval.isHittable, "Running history must render immediately after reopening")
        }
        captureScreen(app, named: "Running long history reopened without a blank viewport")
    }

    private func loadedItems(in detail: XCUIElement) -> Int {
        let value = detail.value as? String ?? ""
        return Int(value.components(separatedBy: "items=").last ?? "") ?? -1
    }

    func testSimulatorKeepsSmallOlderScrollDuringLiveUpdate() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only isolated scroll-position fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)

        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-long-history")]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitForExistence(timeout: 30))
        XCTAssertTrue(app.descendants(matching: .any)["item.long-9-0"].waitForExistence(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        message.tap(); message.typeText("[delayed-input] Keep the reading position")
        let send = app.buttons["task.send"]
        XCTAssertTrue(send.waitForExistence(timeout: 10)); send.tap()
        let keyboardHidden = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: app.keyboards.firstMatch
        )
        wait(for: [keyboardHidden], timeout: 5)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.interrupt.").waitForExistence(timeout: 10))
        let initialDetailValue = detail.value as? String ?? ""
        XCTAssertFalse(initialDetailValue.isEmpty)

        // Use an explicit low velocity: XCTest .slow is 250px/s and can coast beyond 80pt.
        // This drag toward older content must end less than 80pt from the bottom.
        // The subsequent turn update must leave the reader detached at that point.
        let latestButton = app.buttons["task.latest"]
        if latestButton.exists {
            latestButton.tap()
        }
        let atBottom = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: latestButton)
        wait(for: [atBottom], timeout: 5)
        let anchor = app.descendants(matching: .any)["item.long-9-0"]
        let beforeDrag = anchor.frame.minY
        // Start in the horizontal padding so the press cannot begin text selection.
        let start = detail.coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.55))
        start.press(forDuration: 0.4, thenDragTo: start.withOffset(CGVector(dx: 0, dy: 25)),
                    withVelocity: XCUIGestureVelocity(rawValue: 40), thenHoldForDuration: 0.5)
        let readingPosition = anchor.frame.minY
        XCTAssertGreaterThan(readingPosition - beforeDrag, 5)
        XCTAssertLessThan(readingPosition - beforeDrag, 80)
        XCTAssertFalse(latestButton.exists, "The small drag must stay within the near-bottom threshold")
        try simulatorFixture("release-inputs")

        let updated = expectation(for: NSPredicate(format: "value != %@", initialDetailValue), evaluatedWith: detail)
        wait(for: [updated], timeout: 30)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 20))
        captureScreen(app, named: "Small older scroll remains detached after live update")
        XCTAssertEqual(anchor.frame.minY, readingPosition, accuracy: 3,
                       "Receiving new items must preserve the visible message position")
    }

    func testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only repeated-turn history fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("repeated-history")
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.refresh"].tap()
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-repeated-history")]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitForExistence(timeout: 15)); composer.tap()
        composer.typeText("[success] Preserve both persisted responses")
        app.buttons["task.send"].tap()
        let latest = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["task.detail"].waitForExistence(timeout: 30))
        XCTAssertTrue(app.descendants(matching: .any)["item.duplicate-history-new"].waitForExistence(timeout: 20))
        XCTAssertTrue(latest.waitForExistence(timeout: 20),
                      "Reopening history must retain the latest AI response")
        XCTAssertTrue(app.descendants(matching: .any)["item.duplicate-history-old"].waitForExistence(timeout: 20),
                      "Opening history must retain the older AI response")
    }

    func testSimulatorKeepsEarlierAnswersBetweenFollowupsWhenReopening() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only followup history fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[followups] Keep earlier replies")
        let latest = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        for id in ["item.history-answer-1", "item.history-answer-2"] {
            let answer = app.descendants(matching: .any)[id]
            XCTAssertTrue(answer.waitForExistence(timeout: 10), "Earlier replies must remain outside collapsed work")
        }
        captureScreen(app, named: "Earlier answers between followups")
    }

    func testSimulatorRendersVisualizationAndReopensIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] [visualize] Compare twelve icons")
        let expand = app.buttons["visualize.expand"]
        XCTAssertTrue(expand.waitForExistence(timeout: 30))
        let message = app.descendants(matching: .any)["task.message"]
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        captureScreen(app, named: "Visualization loaded with twelve choices")
        XCTAssertTrue(app.buttons["task.send"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        func verify() {
            let second = app.webViews.switches["02 ブランチ＋チェックをプレビュー"]
            XCTAssertTrue(second.waitForExistence(timeout: 20))
            second.tap()
            XCTAssertEqual(second.value as? String, "1")
            XCTAssertTrue(app.webViews.staticTexts["02 · ブランチ＋チェック"].waitForExistence(timeout: 5))
        }
        verify()
        captureScreen(app, named: "Visualization selected second icon")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(expand.waitForExistence(timeout: 20))
        verify()
        captureScreen(app, named: "Reopened visualization selected second icon")
    }

    func testSimulatorRendersMarkdownTableAndReopensIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] [markdown-table] Render the table")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 30))
        let message = app.descendants(matching: .any)["task.message"]
        let emptyValue = try XCTUnwrap(message.placeholderValue)
        let cleared = expectation(for: NSPredicate(format: "value == %@", emptyValue), evaluatedWith: message)
        wait(for: [cleared], timeout: 10)
        XCTAssertTrue(app.buttons["task.send"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        verifyMarkdownTable(app)
        captureScreen(app, named: "Japanese Markdown table right columns")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(message.waitForExistence(timeout: 20))
        verifyMarkdownTable(app)
        captureScreen(app, named: "Reopened Japanese Markdown table")
    }

    private func verifyMarkdownTable(_ app: XCUIApplication) {
        let table = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(table.waitForExistence(timeout: 15))
        let header = app.textViews["markdown.cell.0.0.0"]
        let first = app.textViews["markdown.cell.0.1.0"]
        let second = app.textViews["markdown.cell.0.2.0"]
        XCTAssertEqual(header.value as? String, "構成")
        XCTAssertEqual(first.value as? String, "Codexハーネス＋Claude接続")
        XCTAssertEqual(second.value as? String, "Codex／Claude Codeを並列接続")
        XCTAssertEqual(header.frame.minX, first.frame.minX, accuracy: 1)
        XCTAssertEqual(first.frame.minX, second.frame.minX, accuracy: 1)
        XCTAssertGreaterThan(second.frame.minY, first.frame.maxY)
        XCTAssertTrue(first.isHittable)
        let firstRowY = first.frame.minY
        let headerHeight = header.frame.height
        captureScreen(app, named: "Japanese Markdown table first column")
        table.swipeLeft()
        table.swipeLeft()
        let burden = app.textViews["markdown.cell.0.1.2"]
        XCTAssertTrue(burden.isHittable)
        XCTAssertEqual(burden.value as? String, "通信変換、モデルの挙動、サブスク認証との適合を検証する必要")
        XCTAssertGreaterThan(burden.frame.height, headerHeight * 2)
        XCTAssertEqual(app.textViews["markdown.cell.0.2.2"].value as? String, "両者の機能差をBexが吸収する必要")
        XCTAssertEqual(firstRowY, burden.frame.minY, accuracy: 1)
    }

    func testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only isolated Markdown stream fixture")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[long-markdown] Render the complete stream")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Keep this draft")
        XCTAssertEqual(message.value as? String, "Keep this draft")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(message.waitForExistence(timeout: 20))
        XCTAssertEqual(message.value as? String, "Keep this draft")
        let completed = app.textViews.matching(NSPredicate(format: "value CONTAINS %@", "MARKDOWN_STREAM_COMPLETE"))
            .firstMatch
        XCTAssertTrue(completed.waitForExistence(timeout: 30))
        captureScreen(app, named: "Long Markdown stream completed with draft preserved")
    }

    func testSimulatorReopensCompletedHistoryCollapsed() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[history] Reopen history")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").waitForExistence(timeout: 10))

        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 10))
        let newestTask = prefixedElement(app, prefix: "tasks.row.")
        XCTAssertTrue(newestTask.waitForExistence(timeout: 10))
        newestTask.tap()

        XCTAssertTrue(app.descendants(matching: .any)["task.detail"].waitForExistence(timeout: 15))
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
        XCTAssertTrue(app.buttons["task.diff"].waitForExistence(timeout: 10))
        captureScreen(app, named: "Reference conversation layout")
        let activity = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        activity.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitForExistence(timeout: 5))
        captureScreen(app, named: "Expanded work rows")
        let command = app.buttons.matching(NSPredicate(format: "label == %@", "./gradlew test")).firstMatch
        command.tap()
        let fullOutput = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "DEFERRED_DETAIL_FULL_TEXT"))
            .firstMatch
        XCTAssertTrue(
            fullOutput.waitForExistence(timeout: 10),
            "Reopening must preserve the full completed output"
        )
        captureScreen(app, named: "Completed activity loaded")
        command.tap()
        // Activity headers now leave the accessibility tree when virtualized.
        // Scroll back to the real control before collapsing its work rows.
        for _ in 0 ..< 10 {
            if activity.exists, activity.isHittable {
                break
            }
            app.descendants(matching: .any)["task.detail"].swipeDown()
        }
        XCTAssertTrue(activity.exists && activity.isHittable)
        activity.tap()
        app.buttons["回答をコピー"].firstMatch.tap()
        XCTAssertTrue(app.buttons["コピーしました"].firstMatch.exists)

        try verifyUncachedHistoryDetails(app, fullOutput: fullOutput)
    }

    private func verifyUncachedHistoryDetails(_ app: XCUIApplication, fullOutput: XCUIElement) throws {
        // A previously unseen conversation must fetch deferred details instead of using the live cache.
        try useSimulatorListFixture("completed-history")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let persisted = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-thread-persisted")]
        XCTAssertTrue(persisted.waitForExistence(timeout: 20)); persisted.tap()
        let persistedActivity = app.buttons["turn.activity.fixture-turn-persisted"]
        XCTAssertTrue(persistedActivity.waitForExistence(timeout: 20))
        XCTAssertFalse(app.buttons["item.fixture-command-persisted"].exists)
        persistedActivity.tap()
        let persistedCommand = app.buttons["item.fixture-command-persisted"]
        XCTAssertTrue(persistedCommand.waitForExistence(timeout: 10)); persistedCommand.tap()
        XCTAssertTrue(fullOutput.waitForExistence(timeout: 10), "Uncached history must fetch the full deferred body")
        captureScreen(app, named: "Uncached deferred activity loaded")
    }
}
