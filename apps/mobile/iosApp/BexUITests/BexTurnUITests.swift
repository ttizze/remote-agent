import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorKeepsLatestVisibleAcrossRepeatedLongHistorySubmissions() throws {
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-long-history")]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.long-9-0"].waitForExistence(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        let latestButton = app.buttons["task.latest"]
        let approval = app.buttons["request.accept"]
        let answers = app.textViews.matching(NSPredicate(format: "value == %@", "シミュレータで完了しました。"))
        for attempt in 0 ..< 3 {
            // A multiline draft makes both the keyboard and composer shrink on send.
            let prompt = "[approval] Followup \(attempt)\n" + String(
                repeating: "Keep the latest message visible.\n",
                count: 6
            )
            message.tap(); message.typeText(prompt)
            XCTAssertTrue(app.keyboards.firstMatch.exists)
            app.buttons["task.send"].tap()
            let keyboardHidden = expectation(for: NSPredicate(format: "exists == false"),
                                             evaluatedWith: app.keyboards.firstMatch)
            wait(for: [keyboardHidden], timeout: 5)
            XCTAssertTrue(approval.waitForExistence(timeout: 20))
            XCTAssertTrue(approval.isHittable, "New rows must stay visible after the keyboard and draft shrink")
            XCTAssertFalse(latestButton.exists, "Sending at the bottom must continue following the latest content")
            captureScreen(app, named: "Long history submission \(attempt) remains visible")
            approval.tap()
            let finished = expectation(for: NSPredicate { _, _ in
                !self.prefixedButton(app, prefix: "turn.interrupt.").exists &&
                    answers.allElementsBoundByIndex.contains { $0.isHittable }
            }, evaluatedWith: app)
            wait(for: [finished], timeout: 20)
            XCTAssertFalse(latestButton.exists, "The completed answer must remain at the bottom")
        }
    }

    func testSimulatorSelectsPluginAndSkillFromComposer() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let input = app.textFields["task.message"]
        XCTAssertTrue(input.waitForExistence(timeout: 10)); input.tap()
        input.typeText("/fixture")
        let skill = app.buttons["composer.invocation.fixture-review"]
        XCTAssertTrue(skill.waitForExistence(timeout: 15)); skill.tap()
        XCTAssertEqual(input.value as? String, "$fixture-review ")
        input.typeText("@Fixture")
        let plugin = app.buttons["composer.invocation.Fixture Plugin"]
        XCTAssertTrue(plugin.waitForExistence(timeout: 15)); plugin.tap()
        XCTAssertEqual(input.value as? String, "$fixture-review @Fixture Plugin ")
        input.typeText("[success] Use the selected tools")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 30))
        XCTAssertFalse((input.value as? String ?? "").contains("fixture-review"))
    }

    func testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] [selection] Verify assistant selection")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 30))
        let text = answer.textViews["message.assistant-text"]
        XCTAssertTrue(text.waitForExistence(timeout: 5))
        XCTAssertTrue((text.value as? String ?? "").contains("Second paragraph"))
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        XCTAssertFalse(
            app.textViews["message.text-selection"].exists,
            "Assistant selection must stay in the conversation"
        )
        let copy = app.menuItems["コピー"]
        XCTAssertTrue(copy.waitForExistence(timeout: 5)); copy.tap()
        let message = app.textFields["task.message"]
        message.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitForExistence(timeout: 5)); paste.tap()
        XCTAssertEqual(message.value as? String, "Needle")
        text.tap()
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        let add = app.menuItems["チャットに追加"]
        XCTAssertTrue(add.waitForExistence(timeout: 5)); add.tap()
        let expected = "Needle\n\n> Needle\n\n"
        let inserted = expectation(for: NSPredicate(format: "value == %@", expected), evaluatedWith: message)
        wait(for: [inserted], timeout: 5)
        captureScreen(app, named: "Assistant selection added to an existing draft")
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.codex:")
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        let restored = reopened.textFields["task.message"]
        XCTAssertTrue(restored.waitForExistence(timeout: 10))
        XCTAssertEqual(restored.value as? String, expected)
    }

    func testSimulatorOpensSideChatWithoutLosingOriginalDraft() throws {
        try verifyAssistantSideChat(retryRead: false)
    }

    func testSimulatorRetriesSideChatPreparationWithoutLosingOriginalDraft() throws {
        try verifyAssistantSideChat(retryRead: true)
    }

    private func verifyAssistantSideChat(retryRead: Bool) throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] [selection] Original conversation")
        let original = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(original.waitForExistence(timeout: 30))
        let originalID = original.identifier
        let message = app.textFields["task.message"]
        message.tap(); message.typeText("Keep my original draft")
        if !retryRead {
            verifyEmptySideChatTab(app)
        }
        let text = original.textViews["message.assistant-text"]
        text.tap()
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        if retryRead {
            _ = try simulatorFixture("fail-next-thread-start")
        }
        openAssistantSelectionSideChat(app)
        if retryRead {
            let retry = app.buttons["再試行"]
            XCTAssertTrue(retry.waitForExistence(timeout: 10))
            retry.tap()
        }
        let sideMessage = app.textFields["task.message"]
        waitForDraftText(sideMessage, text: "> Needle\n\n")
        if !retryRead {
            sideMessage.tap()
            sideMessage.coordinate(withNormalizedOffset: CGVector(dx: 0.95, dy: 0.95)).tap()
            sideMessage.typeText("Keep my side draft")
            app.buttons["workbench.terminal"].tap()
            XCTAssertTrue(app.buttons["workbench.sideChat"].exists)
            app.buttons["workbench.sideChat"].tap()
            waitForDraftText(sideMessage, text: "> Needle\n\nKeep my side draft")
            closeWorkbench(app)
            XCTAssertTrue(app.descendants(matching: .any)[originalID].waitForExistence(timeout: 15))
            waitForDraftText(message, text: "Keep my original draft")
            XCTAssertFalse(app.staticTexts["notice"].exists)
            app.buttons["task.tools"].tap()
            waitForDraftText(sideMessage, text: "> Needle\n\nKeep my side draft")
            closeWorkbench(app)
            return
        }
        try verifySideChatSubmissionAndRestore(app, originalID: originalID)
    }

    private func verifyEmptySideChatTab(_ app: XCUIApplication) {
        app.buttons["task.tools"].tap()
        let tab = app.buttons["workbench.sideChat"]
        XCTAssertTrue(tab.waitForExistence(timeout: 5)); tab.tap()
        let input = app.textFields["task.message"]
        XCTAssertTrue(input.waitForExistence(timeout: 15))
        waitForDraftText(input, text: input.placeholderValue ?? "")
        closeWorkbench(app)
        waitForDraftText(app.textFields["task.message"], text: "Keep my original draft", timeout: 10)
    }

    private func waitForDraftText(_ input: XCUIElement, text: String, timeout: TimeInterval = 15) {
        let ready = expectation(
            for: NSPredicate(format: "exists == true AND value == %@", text), evaluatedWith: input
        )
        wait(for: [ready], timeout: timeout)
    }

    private func openAssistantSelectionSideChat(_ app: XCUIApplication) {
        let side = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "サイドチャットで質問")).firstMatch
        if !side.exists {
            let forward = app.buttons["Forward"]
            XCTAssertTrue(forward.waitForExistence(timeout: 5)); forward.tap()
        }
        XCTAssertTrue(side.waitForExistence(timeout: 5)); side.tap()
        XCTAssertTrue(app.buttons["workbench.sideChat"].waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertTrue(app.buttons["workbench.terminal"].exists)
        XCTAssertTrue(app.buttons["workbench.browser"].exists)
        XCTAssertTrue(app.buttons["workbench.files"].exists)
    }

    private func verifySideChatSubmissionAndRestore(
        _ app: XCUIApplication,
        originalID: String
    ) throws {
        let sideMessage = app.textFields["task.message"]
        sideMessage.tap()
        sideMessage.coordinate(withNormalizedOffset: CGVector(dx: 0.95, dy: 0.95)).tap()
        sideMessage.typeText("[success] Explain this selection")
        XCTAssertEqual(
            sideMessage.value as? String, "> Needle\n\n[success] Explain this selection", app.debugDescription
        )
        app.buttons["task.send"].tap()
        let final = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item.fixture-final-")).firstMatch
        XCTAssertTrue(final.waitForExistence(timeout: 30))
        let sideID = final.identifier
        XCTAssertNotEqual(sideID, originalID)
        XCTAssertTrue(app.staticTexts["> Needle\n\n[success] Explain this selection"].exists, app.debugDescription)
        let running = app.buttons
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "turn.interrupt.")).firstMatch
        let completed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: running)
        wait(for: [completed], timeout: 10)
        XCTAssertEqual(sideMessage.value as? String, sideMessage.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Selected quote submitted in side chat")
        let message = app.textFields["task.message"]
        closeWorkbench(app)
        XCTAssertTrue(app.descendants(matching: .any)[originalID].waitForExistence(timeout: 15))
        waitForDraftText(message, text: "Keep my original draft", timeout: 10)
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let number = try XCTUnwrap(sideID.split(separator: "-").dropLast().last)
        let row = try reopened.descendants(matching: .any)[simulatorConversationElementID("fixture-thread-\(number)")]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(reopened.descendants(matching: .any)[sideID].waitForExistence(timeout: 20))
        XCTAssertTrue(reopened.staticTexts["> Needle\n\n[success] Explain this selection"].exists)
    }

    func testSimulatorCopiesOnlySelectedMessageText() throws {
        let app = try connectedSimulatorApp()
        let text = "Needle Alpha Bravo [success]"
        try startSimulatorConversation(app, promptText: text)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 30))
        app.staticTexts[text].press(forDuration: 1.2)
        let fullCopy = app.buttons["コピー"]
        XCTAssertTrue(fullCopy.waitForExistence(timeout: 10)); fullCopy.tap()
        let composer = app.textFields["task.message"]
        composer.press(forDuration: 1.2)
        let fullPaste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(fullPaste.waitForExistence(timeout: 5)); fullPaste.tap()
        let fullText = expectation(for: NSPredicate(format: "value == %@", text), evaluatedWith: composer)
        wait(for: [fullText], timeout: 5)
        composer.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: text.count))
        XCTAssertEqual(composer.value as? String, composer.placeholderValue)
        app.staticTexts[text].press(forDuration: 1.2)
        let select = app.buttons["テキストを選択"]
        XCTAssertTrue(select.waitForExistence(timeout: 5)); select.tap()
        let selection = app.textViews["message.text-selection"]
        XCTAssertTrue(selection.waitForExistence(timeout: 5))
        XCTAssertEqual(selection.value as? String, text)
        selection.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: 40, dy: 27)).press(forDuration: 1.2)
        let copy = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Copy", "コピー"])).firstMatch
        XCTAssertTrue(copy.waitForExistence(timeout: 5)); copy.tap()
        app.buttons["完了"].tap()
        composer.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitForExistence(timeout: 5)); paste.tap()
        let selectedText = expectation(for: NSPredicate(format: "value == %@", "Needle"), evaluatedWith: composer)
        wait(for: [selectedText], timeout: 5)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Only the selected word was copied")
    }

    func testSimulatorKeepsChatUnassignedAfterSendingAndReopening() throws {
        let app = try connectedSimulatorApp()
        try useAutomaticWorktrees(app)
        app.buttons["tasks.new.chat"].tap()
        XCTAssertEqual(app.buttons["task.folder"].label, "フォルダ: チャット")
        let prompt = app.textFields["task.message"]
        prompt.tap(); prompt.typeText("[success] Keep this chat unassigned")
        app.buttons["task.send"].tap()
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 30))
        let answerID = answer.identifier
        let completed = expectation(
            for: NSPredicate(format: "label CONTAINS %@", "件の過去のメッセージ"),
            evaluatedWith: prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        )
        wait(for: [completed], timeout: 10)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertTrue(app.staticTexts["[success] Keep this chat unassigned"].exists)
        XCTAssertEqual(prompt.value as? String, prompt.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        app.buttons["task.new"].tap()
        XCTAssertEqual(app.buttons["task.folder"].label, "フォルダ: チャット")
        app.terminate()
        _ = try connectedSimulatorApp(expandProject: false)
        let number = try XCTUnwrap(answerID.split(separator: "-").dropLast().last)
        let row = try app.descendants(matching: .any)[simulatorConversationElementID("fixture-thread-\(number)")]
        XCTAssertTrue(row.waitForExistence(timeout: 20), "The chat must be outside collapsed projects")
        row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 20))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Unassigned chat after reopening")
        app.buttons["task.new"].tap()
        XCTAssertEqual(app.buttons["task.folder"].label, "フォルダ: チャット")
    }

    func testSimulatorApprovalEditorAndDraftSurviveReconnect() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] Verify approvals")
        let accept = app.buttons["request.accept"]
        XCTAssertTrue(accept.waitForExistence(timeout: 15))
        accept.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 15))

        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Keep this draft")
        openFiles(app)
        let file = app.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitForExistence(timeout: 10)); file.tap()
        let editor = app.textViews["file.editor"]
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        editor.tap(); editor.typeText("Saved from iPhone\n")
        XCTAssertTrue(
            (editor.value as? String ?? "").contains("Saved from iPhone"),
            "Editor: \(String(describing: editor.value))"
        )
        let editable = expectation(
            for: NSPredicate(format: "isEnabled == true"),
            evaluatedWith: app.buttons["file.save"]
        )
        wait(for: [editable], timeout: 10)
        app.buttons["file.save"].tap()
        let saved = expectation(for: NSPredicate(format: "isEnabled == false"), evaluatedWith: app.buttons["file.save"])
        wait(for: [saved], timeout: 10)
        app.buttons["file.close"].tap()
        file.tap()
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        XCTAssertTrue((editor.value as? String ?? "").contains("Saved from iPhone"))
        app.buttons["file.close"].tap()
        app.buttons["変更済み"].tap()
        let savedDiff = app.staticTexts
            .matching(NSPredicate(format: "label BEGINSWITH '+' AND label CONTAINS 'Saved from iPhone'")).firstMatch
        XCTAssertTrue(savedDiff.waitForExistence(timeout: 10))
        closeWorkbench(app)
        XCTAssertEqual(message.value as? String, "Keep this draft")

        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.codex:")
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        let restored = reopened.descendants(matching: .any)["task.message"]
        XCTAssertTrue(restored.waitForExistence(timeout: 10))
        XCTAssertEqual(restored.value as? String, "Keep this draft")
    }

    func testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] [deferred-steer] Hold this turn")
        XCTAssertTrue(app.buttons["request.accept"].waitForExistence(timeout: 15))
        let command = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        let message = app.textFields["task.message"]
        message.tap(); message.typeText("Show this additional input immediately")
        app.buttons["task.send"].tap()
        let sent = app.staticTexts["Show this additional input immediately"]
        XCTAssertTrue(sent.waitForExistence(timeout: 2), "Accepted input must be visible while its native echo is held")
        XCTAssertTrue(sent.isHittable, "Additional input must remain visible at the latest position")
        XCTAssertGreaterThan(sent.frame.minY, command.frame.minY, "Additional input was moved above the preceding work")
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-steer-recorded-").exists)
        captureScreen(app, named: "Accepted additional input before native echo")
        try simulatorFixture("release-inputs")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-steer-recorded-").waitForExistence(timeout: 10))
        XCTAssertEqual(
            app.staticTexts.matching(NSPredicate(format: "label == %@", "Show this additional input immediately"))
                .count,
            1
        )
        prefixedButton(app, prefix: "turn.interrupt.").tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch
            .waitForExistence(timeout: 15))
        XCTAssertEqual(
            app.staticTexts.matching(NSPredicate(format: "label == %@", "Show this additional input immediately"))
                .count,
            1
        )
        XCTAssertFalse(app.buttons["request.accept"].exists)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        command.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitForExistence(timeout: 5))
        XCTAssertEqual(message.value as? String, message.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
    }
}
