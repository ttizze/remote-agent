import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorReopensRunningLongHistoryWithoutBlankViewport() throws {
        let app = try connectedSimulatorApp()
        let conversationId = try useSimulatorConversationFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.codex:\(conversationId)"]
        XCTAssertTrue(row.waitUntilExists(timeout: 30)); row.tap()
        let latest = app.descendants(matching: .any)["item.long-14-0"]
        XCTAssertTrue(latest.waitUntilExists(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("[approval] Reopen while this turn is running")
        app.buttons["task.send"].tap()
        let running = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(running.waitUntilExists(timeout: 10))
        let approval = app.buttons["request.accept"]
        XCTAssertTrue(approval.waitUntilExists(timeout: 10))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        XCTAssertTrue(running.waitUntilExists(timeout: 10))
        XCTAssertTrue(approval.waitUntilExists(timeout: 10))
        XCTAssertTrue(approval.isHittable, "Running history must render immediately after reopening")
        captureScreen(app, named: "Running long history reopened without a blank viewport")
    }

    func testSimulatorKeepsSmallOlderScrollDuringLiveUpdate() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only isolated scroll-position fixture")
        #endif
        let app = try connectedSimulatorApp()
        let conversationId = try useSimulatorConversationFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)

        let row = app.descendants(matching: .any)["tasks.row.codex:\(conversationId)"]
        XCTAssertTrue(row.waitUntilExists(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitUntilExists(timeout: 30))
        XCTAssertTrue(app.descendants(matching: .any)["item.long-14-0"].waitUntilExists(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(message.waitUntilExists(timeout: 10))
        message.tap(); message.typeText("[delayed-input] Keep the reading position")
        let send = app.buttons["task.send"]
        XCTAssertTrue(send.waitUntilExists(timeout: 10)); send.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))
        let running = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(running.waitUntilExists(timeout: 10))
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
        let anchor = app.descendants(matching: .any)["item.long-14-0"]
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
        // The completed answer is below this detached viewport and is deliberately not materialized.
        // Observe completion through the visible composer, then verify the same reading position.
        XCTAssertTrue(running.waitForNonExistence(timeout: 20))
        captureScreen(app, named: "Small older scroll remains detached after live update")
        XCTAssertEqual(anchor.frame.minY, readingPosition, accuracy: 3,
                       "Receiving new items must preserve the visible message position")
        if latestButton.exists {
            latestButton.tap()
        }
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 20))
    }

    func testSimulatorRendersVisualizationAndReopensIt() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[success] [visualize] Compare twelve icons")
        let expand = app.buttons["visualize.expand"]
        XCTAssertTrue(expand.waitUntilExists(timeout: 30))
        let message = app.descendants(matching: .any)["task.message"]
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        captureScreen(app, named: "Visualization loaded with twelve choices")
        XCTAssertTrue(app.buttons["task.send"].waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        func verify() {
            let second = app.webViews.switches["02 ブランチ＋チェックをプレビュー"]
            XCTAssertTrue(second.waitUntilExists(timeout: 20))
            second.tap()
            XCTAssertEqual(second.value as? String, "1")
            XCTAssertTrue(app.webViews.staticTexts["02 · ブランチ＋チェック"].waitUntilExists(timeout: 5))
        }
        verify()
        captureScreen(app, named: "Visualization selected second icon")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.codex:bex-task:")
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        XCTAssertTrue(expand.waitUntilExists(timeout: 20))
        verify()
        captureScreen(app, named: "Reopened visualization selected second icon")
    }

    func testSimulatorRendersMarkdownTableAndReopensIt() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[success] [markdown-table] Render the table")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitUntilExists(timeout: 30))
        let message = app.descendants(matching: .any)["task.message"]
        let emptyValue = try XCTUnwrap(message.placeholderValue)
        let cleared = expectation(for: NSPredicate(format: "value == %@", emptyValue), evaluatedWith: message)
        wait(for: [cleared], timeout: 10)
        XCTAssertTrue(app.buttons["task.send"].waitUntilExists(timeout: 10))
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        verifyMarkdownTable(app)
        captureScreen(app, named: "Japanese Markdown table right columns")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.codex:bex-task:")
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        XCTAssertTrue(message.waitUntilExists(timeout: 20))
        verifyMarkdownTable(app)
        captureScreen(app, named: "Reopened Japanese Markdown table")
    }

    private func verifyMarkdownTable(_ app: XCUIApplication) {
        let table = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(table.waitUntilExists(timeout: 15))
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
        // Measure after the conversation settles, so only the horizontal swipes can move the row.
        waitForStableFrame(first)
        let firstRowY = first.frame.minY
        let headerHeight = header.frame.height
        captureScreen(app, named: "Japanese Markdown table first column")
        table.swipeLeft()
        table.swipeLeft()
        let burden = app.textViews["markdown.cell.0.1.2"]
        waitForStableFrame(burden)
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
        startSimulatorConversation(app, promptText: "[long-markdown] Render the complete stream")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        let completed = app.textViews.matching(NSPredicate(format: "value CONTAINS %@", "MARKDOWN_STREAM_COMPLETE"))
            .firstMatch
        focusComposerKeepingLatestVisible(in: app, latestResponse: completed)
        message.typeText("Keep this draft")
        waitForDraftText(message, text: "Keep this draft")
        // Empty space beside the messages dismisses the keyboard too.
        app.descendants(matching: .any)["task.detail"].coordinate(withNormalizedOffset: CGVector(dx: 0.02, dy: 0.3))
            .tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.codex:bex-task:")
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        XCTAssertTrue(message.waitUntilExists(timeout: 20))
        waitForDraftText(message, text: "Keep this draft")
        XCTAssertTrue(completed.waitUntilExists(timeout: 30))
        let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: completed)
        wait(for: [visible], timeout: 5)
        XCTAssertFalse(app.buttons["task.latest"].exists,
                       "Streaming growth of the same row must keep its final text visible")
        assertConversationTextMargins(completed, in: app)
        captureScreen(app, named: "Long Markdown stream completed with draft preserved")
        tapStatusBar(app)
        let viewport = app.descendants(matching: .any)["task.detail"]
        XCTAssertGreaterThanOrEqual(viewport.frame.maxY, message.frame.maxY,
                                    "Conversation must draw behind the translucent composer")
        captureScreen(app, named: "Conversation behind diff and composer without keyboard")
        let code = app.textViews.matching(NSPredicate(format: "value CONTAINS %@", "Markdown stream fixture"))
            .firstMatch
        XCTAssertTrue(code.waitUntilExists(timeout: 10))
        message.tap()
        // Place the caret after the restored draft before appending.
        message.coordinate(withNormalizedOffset: CGVector(dx: 0.99, dy: 0.5)).tap()
        message.typeText(" while reading")
        waitForDraftText(message, text: "Keep this draft while reading")
        XCTAssertGreaterThanOrEqual(viewport.frame.maxY, message.frame.maxY)
        captureScreen(app, named: "Conversation behind diff and composer with keyboard")
        let latest = app.buttons["task.latest"]
        XCTAssertTrue(latest.exists, "Editing must retain the older reading position")
        latest.tap()
        let returned = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: completed)
        wait(for: [returned], timeout: 5)
        assertConversationTextMargins(completed, in: app)
    }

    func focusComposerKeepingLatestVisible(in app: XCUIApplication, latestResponse: XCUIElement?) {
        app.descendants(matching: .any)["task.message"].tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        if let latestResponse {
            assertConversationTextMargins(latestResponse, in: app)
            XCTAssertTrue(latestResponse.isHittable, "Opening the keyboard must keep the conversation visible")
        }
        XCTAssertFalse(app.buttons["task.latest"].exists,
                       "Opening the keyboard must keep following the latest message")
    }

    func assertConversationTextMargins(_ text: XCUIElement, in app: XCUIApplication,
                                       file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(text.waitUntilExists(timeout: 10), file: file, line: line)
        waitForStableFrame(text)
        let viewport = app.descendants(matching: .any)["task.detail"].frame
        XCTAssertEqual(text.frame.minX, viewport.minX + 16, accuracy: 1, file: file, line: line)
        XCTAssertEqual(text.frame.maxX, viewport.maxX - 16, accuracy: 1, file: file, line: line)
    }

    func testSimulatorReopensCompletedHistoryCollapsed() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[history] Reopen history")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").waitUntilExists(timeout: 10))

        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 10))
        let newestTask = prefixedElement(app, prefix: "tasks.row.codex:bex-task:")
        XCTAssertTrue(newestTask.waitUntilExists(timeout: 10))
        newestTask.tap()

        XCTAssertTrue(app.descendants(matching: .any)["task.detail"].waitUntilExists(timeout: 15))
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
        XCTAssertTrue(app.buttons["task.diff"].waitUntilExists(timeout: 10))
        captureScreen(app, named: "Reference conversation layout")
        let activity = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        activity.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitUntilExists(timeout: 5))
        captureScreen(app, named: "Expanded work rows")
        let command = app.buttons.matching(NSPredicate(format: "label == %@", "./gradlew test")).firstMatch
        command.tap()
        let fullOutput = app.textViews.matching(NSPredicate(format: "value CONTAINS %@", "DEFERRED_DETAIL_FULL_TEXT"))
            .firstMatch
        XCTAssertTrue(
            fullOutput.waitUntilExists(timeout: 10),
            "Reopening must preserve the full completed output"
        )
        captureScreen(app, named: "Completed activity loaded")
        command.tap()
        // Long command output can move the activity header above the viewport.
        for _ in 0 ..< 10 {
            if activity.exists, activity.isHittable {
                break
            }
            app.descendants(matching: .any)["task.detail"].swipeDown()
        }
        XCTAssertTrue(activity.exists && activity.isHittable)
        activity.tap()
        app.buttons["回答をコピー"].firstMatch.tap()
        XCTAssertTrue(
            app.buttons["コピーしました"].firstMatch.waitUntilExists(timeout: 5),
            "Copy must show its confirmation.\n\(app.debugDescription)"
        )
    }
}
