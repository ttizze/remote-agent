import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorSendsWithOneTapWhileReadingHistory() throws {
        let app = try connectedSimulatorApp()
        let id = try useSimulatorConversationFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.codex:\(id)"]
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.long-14-0"].waitUntilExists(timeout: 20))
        let input = app.descendants(matching: .any)["task.message"]
        for cycle in 0 ..< 4 {
            if cycle.isMultiple(of: 2) {
                tapStatusBar(app)
            } else if app.buttons["task.latest"].exists {
                app.buttons["task.latest"].tap()
            }
            input.tap()
            XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
            input.typeText("[delayed-input] Single send tap \(cycle)")
            let send = app.buttons["task.send"]
            XCTAssertTrue(send.isEnabled)
            send.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            captureScreen(app, named: "Single send tap from history \(cycle)")
            let running = prefixedButton(app, prefix: "turn.interrupt.")
            XCTAssertTrue(running.waitUntilExists(timeout: 10),
                          "One tap must send while the keyboard is open; a second tap is never required")
            try simulatorFixture("release-inputs")
            XCTAssertTrue(running.waitForNonExistence(timeout: 20))
        }
    }

    func testSimulatorComposerGrowsToFitWrappedDraft() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[success] [immediate] Verify composer wrapping")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))
        let input = app.descendants(matching: .any)["task.message"]
        input.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        waitForStableFrame(input)
        let singleLineHeight = input.frame.height
        let draft = String(repeating: "Keep this draft while reading the conversation. ", count: 5)
        input.typeText(draft)
        waitForDraftText(input, text: draft)
        waitForStableFrame(input)
        XCTAssertGreaterThan(input.frame.height, singleLineHeight + 20,
                             "Wrapped text must expand the composer vertically")
        captureScreen(app, named: "Wrapped draft expands the native composer")
    }

    func testSimulatorNshConversationSendsAndShowsTheReply() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
        submitSimulatorConversation(app, promptText: "[success] [immediate] Start the simulator conversation")
        let text = NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@",
                               "シミュレータで完了", "シミュレータで完了")
        let reply = app.descendants(matching: .any).matching(text).firstMatch
        XCTAssertTrue(reply.waitUntilExists(timeout: 30), "The NSH conversation did not show the reply")
        captureScreen(app, named: "NSH conversation with its reply")
    }

    func testSimulatorNshConversationShowsInputSentWhileATurnRuns() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
        submitSimulatorConversation(app, promptText: "[delayed-input] Hold this turn")
        let message = app.textFields["task.message"]
        XCTAssertTrue(message.waitUntilExists(timeout: 10))
        message.tap(); message.typeText("Sent while the turn runs")
        let send = app.buttons["task.send"]
        let enabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        wait(for: [enabled], timeout: 10)
        send.tap()
        sleep(4)
        captureScreen(app, named: "NSH conversation after input sent while the turn runs")
        try simulatorFixture("release-inputs")
        sleep(6)
        captureScreen(app, named: "NSH conversation after the held turn finished")
    }

    func testSimulatorCanStartAConversationInAProject() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This isolated conversation-start E2E runs only in the iOS Simulator")
        #endif
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
        let input = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(input.waitUntilExists(timeout: 10))
        waitForStableFrame(input)
        XCTAssertTrue(app.navigationBars["新しいタスク"].exists)
        XCTAssertFalse(app.buttons["task.send"].isEnabled)
        let prompt = app.staticTexts["task.prompt"]
        let folder = app.buttons["task.folder"]
        let environment = app.buttons["task.environment"]
        let emptyChat = app.scrollViews["task.empty"]
        XCTAssertFalse(emptyChat.staticTexts["新しいタスク"].exists)
        XCTAssertTrue(prompt.waitUntilExists(timeout: 10))
        XCTAssertTrue(folder.isHittable)
        XCTAssertTrue(environment.isHittable)
        XCTAssertLessThan(prompt.frame.maxY, folder.frame.minY)
        XCTAssertLessThan(folder.frame.maxY, environment.frame.minY)
        XCTAssertLessThan(environment.frame.maxY, input.frame.minY)
        XCTAssertEqual(folder.frame.midX, emptyChat.frame.midX, accuracy: 2)
        XCTAssertEqual(environment.frame.midX, emptyChat.frame.midX, accuracy: 2)
        XCTAssertEqual((prompt.frame.minY + environment.frame.maxY) / 2, emptyChat.frame.midY, accuracy: 8)
        captureScreen(app, named: "New conversation centers the prompt project and environment above composer")
        submitSimulatorConversation(app, promptText: "[success] [immediate] Start the simulator conversation")

        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitUntilExists(timeout: 15), "Final answer did not stream into the conversation")
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertTrue(app.buttons["task.send"].exists)
        XCTAssertFalse(app.buttons["task.send"].isEnabled, "Sent input must clear after completion")
        XCTAssertFalse((input.value as? String ?? "").contains("Start the simulator conversation"))
        XCTAssertFalse(app.staticTexts["notice"].exists, "Successful completion must not leave an error")
        captureScreen(app, named: "Native composer enclosure and translucent change capsule")
        app.buttons["task.new"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["task.empty"].waitUntilExists(timeout: 10))
        XCTAssertTrue(input.isHittable)
        XCTAssertFalse(app.descendants(matching: .any)["task.detail"].exists)
        XCTAssertTrue(app.navigationBars["新しいタスク"].exists)
    }

    func testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[groups] Inspect grouped live activity")
        let first = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(first.waitUntilExists(timeout: 10))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(app.buttons["回答をコピー"].exists, "Commentary must not show final-answer actions")
        let progress = prefixedElement(app, prefix: "item.fixture-progress-")
        XCTAssertTrue(progress.waitUntilExists(timeout: 20))
        let second = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH %@ AND identifier CONTAINS %@",
            "turn.activity.",
            ":fixture-next-command-"
        )).firstMatch
        XCTAssertTrue(second.waitUntilExists(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-next-command-").exists)
        XCTAssertLessThan(first.frame.minY, progress.frame.minY)
        XCTAssertLessThan(progress.frame.minY, second.frame.minY)
        captureScreen(app, named: "Commands grouped between visible commentary")
        second.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-next-command-").waitUntilExists(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists,
                       "Expanding one group must leave the earlier group collapsed")
        app.buttons.matching(NSPredicate(format: "label == %@", "pwd")).firstMatch.tap()
        XCTAssertTrue(app.textViews.matching(NSPredicate(format: "value == %@", "GROUP_DETAIL_OUTPUT")).firstMatch
            .waitUntilExists(timeout: 5))
        captureScreen(app, named: "Selected command group and command details expanded")
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 20))
        let completed = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        let completion = expectation(
            for: NSPredicate(format: "label CONTAINS %@", "作業しました"), evaluatedWith: completed
        )
        wait(for: [completion], timeout: 10)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-next-command-").exists)
        XCTAssertFalse(progress.exists, "Completed work must hide interim commentary")
        XCTAssertEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "turn.activity.")).count, 1)
        captureScreen(app, named: "Completed work automatically collapsed")
        completed.tap()
        XCTAssertTrue(progress.waitUntilExists(timeout: 5))
    }

    func testSimulatorShowsRetryingStreamErrorThenRecovers() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[retry] Retry stream")

        let retrying = app.staticTexts["プロバイダーを再試行しています（1）"]
        XCTAssertTrue(retrying.waitUntilExists(timeout: 10), "Retrying stream error was not visible")
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))
        let recovered = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: retrying)
        wait(for: [recovered], timeout: 10)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
    }

    func testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[failed] Fail turn")

        XCTAssertTrue(app.staticTexts["プロバイダーエラー"].waitUntilExists(timeout: 15))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        let group = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(group.exists)
        group.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitUntilExists(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-final-").exists)
    }

    func testSimulatorKeepsInputRequestVisibleUntilResolved() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[request] Ask user")

        let request = prefixedElement(app, prefix: "request.")
        XCTAssertTrue(request.waitUntilExists(timeout: 10), "Pending request was not visible")
        XCTAssertTrue(app.staticTexts["質問"].exists)
        let turn = prefixedButton(app, prefix: "turn.interrupt.fixture-turn-")
        XCTAssertTrue(turn.exists)
        let number = try XCTUnwrap(turn.identifier.split(separator: "-").dropLast().last)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let rowIdentifier = try simulatorTaskRowIdentifier("session-identity/fixture-thread-\(number)")
        let waiting = app.descendants(matching: .any)[
            rowIdentifier.replacingOccurrences(of: "tasks.row.", with: "tasks.waiting.")
        ]
        XCTAssertTrue(waiting.waitUntilExists(timeout: 10))
        XCTAssertEqual(waiting.label, "確認待ち")
        let row = app.buttons[rowIdentifier]
        XCTAssertEqual(row.value as? String, "確認待ち")
        row.tap()
        XCTAssertTrue(request.waitUntilExists(timeout: 10))
        let answer = app.textFields["task.message"]
        XCTAssertTrue(answer.waitUntilExists(timeout: 5))
        answer.tap(); answer.typeText("Continue")
        app.buttons["request.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))
        let resolved = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: request)
        wait(for: [resolved], timeout: 10)
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitUntilExists(timeout: 10))
        XCTAssertFalse(waiting.exists)
        XCTAssertNotEqual(row.value as? String, "確認待ち")
    }

    func testSimulatorSendsQuestionChoiceOnTap() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[request] [choice] Tap an answer")

        let choice = app.buttons["続ける"]
        XCTAssertTrue(choice.waitUntilExists(timeout: 10))
        app.buttons["request.toggle"].tap()
        XCTAssertFalse(choice.exists)
        tapWhenStable(app.buttons["request.toggle"])
        XCTAssertTrue(choice.waitUntilExists(timeout: 5))
        choice.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))
        XCTAssertFalse(app.buttons["request.send"].exists)
    }

    func testSimulatorCanSkipQuestion() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only conversation display E2E")
        #endif
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[request] [skip] Skip this question")

        let skip = app.buttons["request.skip"]
        XCTAssertTrue(skip.waitUntilExists(timeout: 10))
        skip.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))
        XCTAssertFalse(skip.exists)
    }
}
