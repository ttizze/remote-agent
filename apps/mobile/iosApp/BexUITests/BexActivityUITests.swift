import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorCanStartAConversationInAProject() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This isolated conversation-start E2E runs only in the iOS Simulator")
        #endif
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let input = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        XCTAssertTrue(input.isHittable)
        XCTAssertFalse(app.staticTexts["新しいタスク"].exists)
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        captureScreen(app, named: "New conversation opens the existing empty chat")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 10))
        try startSimulatorConversation(app, promptText: "[success] Start the simulator conversation")

        let activity = prefixedElement(app, prefix: "turn.activity.fixture-turn-")
        let streamedCommand = prefixedElement(app, prefix: "item.fixture-command-")
        XCTAssertTrue(activity.waitForExistence(timeout: 10), "Streaming activity header did not appear")
        XCTAssertFalse(streamedCommand.exists, "Live commands must stay collapsed until explicitly expanded")
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-commentary-").exists,
                      "Commentary must remain visible outside the work group")

        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 15), "Final answer did not stream into the conversation")
        let commandCollapsed = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: streamedCommand
        )
        wait(for: [commandCollapsed], timeout: 10)
        XCTAssertTrue(
            prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists,
            "Completed work did not become an expandable collapsed summary"
        )
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertTrue(app.buttons["task.send"].exists)
        XCTAssertFalse(app.buttons["task.send"].isEnabled, "Sent input must clear after completion")
        XCTAssertFalse((input.value as? String ?? "").contains("Start the simulator conversation"))
        XCTAssertFalse(app.staticTexts["notice"].exists, "Successful completion must not leave an error")
        app.buttons["task.new"].tap()
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        XCTAssertTrue(input.isHittable)
        XCTAssertFalse(app.descendants(matching: .any)["task.detail"].exists)
        XCTAssertFalse(app.staticTexts["新しいタスク"].exists)
    }

    func testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[groups] Inspect grouped live activity")
        let first = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(first.waitForExistence(timeout: 10))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(app.buttons["回答をコピー"].exists, "Commentary must not show final-answer actions")
        let progress = prefixedElement(app, prefix: "item.fixture-progress-")
        XCTAssertTrue(progress.waitForExistence(timeout: 20))
        let second = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH %@ AND identifier CONTAINS %@",
            "turn.activity.",
            ":fixture-next-command-"
        )).firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-next-command-").exists)
        XCTAssertLessThan(first.frame.minY, progress.frame.minY)
        XCTAssertLessThan(progress.frame.minY, second.frame.minY)
        captureScreen(app, named: "Commands grouped between visible commentary")
        second.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-next-command-").waitForExistence(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists,
                       "Expanding one group must leave the earlier group collapsed")
        app.buttons.matching(NSPredicate(format: "label == %@", "pwd")).firstMatch.tap()
        XCTAssertTrue(app.staticTexts["GROUP_DETAIL_OUTPUT"].waitForExistence(timeout: 5))
        captureScreen(app, named: "Selected command group and command details expanded")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 20))
        let completed = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        let completion = expectation(
            for: NSPredicate(format: "label CONTAINS %@", "件の過去のメッセージ"), evaluatedWith: completed
        )
        wait(for: [completion], timeout: 10)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-next-command-").exists)
        XCTAssertFalse(progress.exists, "Completed work must hide interim commentary")
        XCTAssertEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "turn.activity.")).count, 1)
        captureScreen(app, named: "Completed work automatically collapsed")
        completed.tap()
        XCTAssertTrue(progress.waitForExistence(timeout: 5))
    }

    func testSimulatorShowsRetryingStreamErrorThenRecovers() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[retry] Retry stream")

        let retrying = app.staticTexts["サーバーが混み合っています。再接続しています"]
        XCTAssertTrue(retrying.waitForExistence(timeout: 10), "Retrying stream error was not visible")
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))
        let recovered = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: retrying)
        wait(for: [recovered], timeout: 10)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
    }

    func testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[failed] Fail turn")

        XCTAssertTrue(app.staticTexts["コンテキストの上限に達しました"].waitForExistence(timeout: 15))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        let group = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(group.exists)
        group.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitForExistence(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-final-").exists)
    }

    func testSimulatorKeepsInterruptedWorkCollapsed() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[interrupted] Interrupt turn")

        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch
            .waitForExistence(timeout: 15))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        let group = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(group.exists)
        group.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitForExistence(timeout: 5))
    }

    func testSimulatorKeepsInputRequestVisibleUntilResolved() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[request] Ask user")

        let request = app.staticTexts["回答待ち"]
        XCTAssertTrue(request.waitForExistence(timeout: 10), "Pending request was not visible")
        XCTAssertTrue(app.staticTexts["回答待ち"].exists)
        let answer = app.textFields["request.answer"]
        XCTAssertTrue(answer.waitForExistence(timeout: 5))
        answer.tap(); answer.typeText("Continue")
        app.buttons["回答を送信"].tap()
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
        for _ in 0 ..< 10 {
            detail.swipeDown()
        }

        for prefix in [
            "item.fixture-plan-", "item.fixture-mcp-", "item.fixture-dynamic-",
            "item.fixture-collab-", "item.fixture-subagent-", "item.fixture-web-",
            "item.fixture-image-", "item.fixture-compaction-"
        ] {
            XCTAssertTrue(waitForPrefixedElement(app, prefix: prefix, scrolling: detail), prefix)
        }
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-sleep-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-review-in-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-review-out-").exists)
    }
}
