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
        let row = app.descendants(matching: .any)["tasks.row.fixture-long-history"]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitForExistence(timeout: 30))
        let latest = app.descendants(matching: .any)["item.long-latest-message"]
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: latest)
        wait(for: [visible], timeout: 5)
        captureScreen(app, named: "Long interrupted history at latest message")
        func loadedItems() -> Int {
            let value = detail.value as? String ?? ""
            return Int(value.components(separatedBy: "items=").last ?? "") ?? -1
        }
        let initialItems = 501
        XCTAssertEqual(loadedItems(), initialItems, "Initial history has 500 items plus the preserved opening input")
        for _ in 0 ..< 40 {
            if loadedItems() > initialItems {
                break
            }
            detail.swipeDown(velocity: .fast)
        }
        XCTAssertGreaterThan(loadedItems(), initialItems,
                             "Scrolling upward must load older items without tapping a button")
        XCTAssertFalse(latest.isHittable, "Prepending history must not jump back to the latest message")
        captureScreen(app, named: "Older history loaded by scrolling")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        XCTAssertTrue(latest.isHittable)
    }

    func testSimulatorKeepsSmallOlderScrollDuringLiveUpdate() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only isolated scroll-position fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)

        let row = app.descendants(matching: .any)["tasks.row.fixture-long-history"]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitForExistence(timeout: 30))
        XCTAssertTrue(app.descendants(matching: .any)["item.long-latest-message"].waitForExistence(timeout: 20))
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
        let anchor = app.descendants(matching: .any)["item.long-latest-message"]
        let beforeDrag = anchor.frame.minY
        let start = detail.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.55))
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
        try startSimulatorConversation(app, promptText: "[duplicate] Preserve both persisted responses")
        let latest = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["task.detail"].waitForExistence(timeout: 30))
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
        let row = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        for id in ["item.history-answer-1", "item.history-answer-2"] {
            let answer = app.descendants(matching: .any)[id]
            XCTAssertTrue(answer.waitForExistence(timeout: 10), "Earlier replies must remain outside collapsed work")
        }
        captureScreen(app, named: "Earlier answers between followups")
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
        let row = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(message.waitForExistence(timeout: 20))
        XCTAssertEqual(message.value as? String, "Keep this draft")
        let completed = app.staticTexts.matching(NSPredicate(format: "label == %@", "MARKDOWN_STREAM_COMPLETE"))
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
        let newestTask = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
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
            "Reopening must preserve the completed command output"
        )
        captureScreen(app, named: "Completed activity retained")
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
    }
}
