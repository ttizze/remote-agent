import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorKeepsLatestVisibleAcrossRepeatedLongHistorySubmissions() throws {
        let app = try connectedSimulatorApp()
        let conversationId = try useSimulatorConversationFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.codex:\(conversationId)"]
        XCTAssertTrue(row.waitUntilExists(timeout: 30)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.long-14-0"].waitUntilExists(timeout: 20))
        let message = app.descendants(matching: .any)["task.message"]
        let latestButton = app.buttons["task.latest"]
        let approval = app.buttons["request.accept"]
        let answers = app.textViews.matching(NSPredicate(format: "value == %@", "シミュレータで完了しました。"))
        for attempt in 0 ..< 3 {
            if attempt > 0 {
                app.navigationBars.buttons.element(boundBy: 0).tap()
                XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
                let reopened = expectation(for: NSPredicate { _, _ in
                    answers.allElementsBoundByIndex.last?.isHittable == true
                }, evaluatedWith: app)
                wait(for: [reopened], timeout: 10)
                XCTAssertFalse(latestButton.exists, "Reopening must settle at the latest answer")
            }
            // A multiline draft makes both the keyboard and composer shrink on send.
            let prompt = "[approval] Followup \(attempt)\n" + String(
                repeating: "Keep the latest message visible.\n",
                count: 6
            )
            // The initial interrupted turn keeps its commentary in collapsed work.
            // Check text geometry once a rendered answer exists; always check bottom following.
            let latestResponse: XCUIElement? = try (attempt > 0 ? latestFixtureAnswer(app) : nil)
            focusComposerKeepingLatestVisible(in: app, latestResponse: latestResponse)
            message.typeText(prompt)
            XCTAssertTrue(app.keyboards.firstMatch.exists)
            app.buttons["task.send"].tap()
            XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5))
            XCTAssertTrue(approval.waitUntilExists(timeout: 20))
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
            try assertConversationTextMargins(latestFixtureAnswer(app), in: app)
        }
    }

    private func latestFixtureAnswer(_ app: XCUIApplication) throws -> XCUIElement {
        // Keyboard resizing can recycle older answers, changing the native text view's ordinal.
        let rows = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item.fixture-final-"))
        let id = try XCTUnwrap(rows.allElementsBoundByIndex.last?.identifier)
        return app.descendants(matching: .any)[id].textViews["message.assistant-text"]
    }

    func testSimulatorSelectsPluginAndSkillFromComposer() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
        let input = app.textFields["task.message"]
        XCTAssertTrue(input.waitUntilExists(timeout: 10)); input.tap()
        input.typeText("/fixture")
        let skill = app.buttons["composer.invocation.fixture-review"]
        XCTAssertTrue(skill.waitUntilExists(timeout: 15)); skill.tap()
        XCTAssertEqual(input.value as? String, "$fixture-review ")
        input.typeText("@Fixture")
        let plugin = app.buttons["composer.invocation.Fixture Plugin"]
        XCTAssertTrue(plugin.waitUntilExists(timeout: 15)); plugin.tap()
        XCTAssertEqual(input.value as? String, "$fixture-review @Fixture Plugin ")
        input.typeText("[success] Use the selected tools")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 30))
        XCTAssertFalse((input.value as? String ?? "").contains("fixture-review"))
    }

    func testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[success] [selection] Verify assistant selection")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitUntilExists(timeout: 30))
        let text = answer.textViews["message.assistant-text"]
        XCTAssertTrue(text.waitUntilExists(timeout: 5))
        XCTAssertTrue((text.value as? String ?? "").contains("Second paragraph"))
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        let copy = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Copy", "コピー"])).firstMatch
        XCTAssertTrue(copy.waitUntilExists(timeout: 5)); copy.tap()
        let message = app.textFields["task.message"]
        // Focus expands the idle composer before its edit menu opens.
        message.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        message.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitUntilExists(timeout: 5)); paste.tap()
        XCTAssertEqual(message.value as? String, "Needle")
        text.tap()
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        let add = app.menuItems["チャットに追加"]
        XCTAssertTrue(add.waitUntilExists(timeout: 5)); add.tap()
        let expected = "Needle\n\n> Needle\n\n"
        let inserted = expectation(for: NSPredicate(format: "value == %@", expected), evaluatedWith: message)
        wait(for: [inserted], timeout: 5)
        captureScreen(app, named: "Assistant selection added to an existing draft")
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.bex:")
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        let restored = reopened.textFields["task.message"]
        XCTAssertTrue(restored.waitUntilExists(timeout: 10))
        XCTAssertEqual(restored.value as? String, expected)
    }

    func testSimulatorOpensSideChatWithoutLosingOriginalDraft() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[success] [selection] Original conversation")
        let original = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(original.waitUntilExists(timeout: 30))
        let originalID = original.identifier
        let message = app.textFields["task.message"]
        message.tap(); message.typeText("Keep my original draft")
        app.buttons["task.tools"].tap()
        openWorkbenchTool(app, "sideChat")
        let input = app.textFields["task.message"]
        XCTAssertTrue(input.waitUntilExists(timeout: 15))
        waitForDraftText(input, text: input.placeholderValue ?? "")
        closeWorkbench(app)
        waitForDraftText(message, text: "Keep my original draft", timeout: 10)
        let text = original.textViews["message.assistant-text"]
        text.tap()
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        openAssistantSelectionSideChat(app)
        let sideMessage = app.textFields["task.message"]
        waitForDraftText(sideMessage, text: "> Needle\n\n")
        sideMessage.tap()
        sideMessage.coordinate(withNormalizedOffset: CGVector(dx: 0.95, dy: 0.95)).tap()
        sideMessage.typeText("[success] Explain this selection")
        XCTAssertEqual(
            sideMessage.value as? String, "> Needle\n\n[success] Explain this selection", app.debugDescription
        )
        app.buttons["task.send"].tap()
        let final = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item.fixture-final-")).firstMatch
        XCTAssertTrue(final.waitUntilExists(timeout: 30))
        let sideID = final.identifier
        XCTAssertNotEqual(sideID, originalID)
        let sent = app.textViews.matching(NSPredicate(
            format: "identifier == %@ AND value == %@", "message.user-text", "Needle\n[success] Explain this selection"
        )).firstMatch
        XCTAssertTrue(sent.exists, app.debugDescription)
        let running = app.buttons
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "turn.interrupt.")).firstMatch
        let completed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: running)
        wait(for: [completed], timeout: 10)
        XCTAssertEqual(sideMessage.value as? String, sideMessage.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Selected quote submitted in side chat")
        closeWorkbench(app)
        XCTAssertTrue(app.descendants(matching: .any)[originalID].waitUntilExists(timeout: 15))
        waitForDraftText(message, text: "Keep my original draft", timeout: 10)
    }

    private func openAssistantSelectionSideChat(_ app: XCUIApplication) {
        let side = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "サイドチャットで質問")).firstMatch
        if !side.exists {
            let forward = app.buttons.matching(NSPredicate(format: "label IN %@", ["Forward", "進む"])).firstMatch
            XCTAssertTrue(forward.waitUntilExists(timeout: 5)); forward.tap()
        }
        XCTAssertTrue(side.waitUntilExists(timeout: 5)); side.tap()
        XCTAssertTrue(app.navigationBars["サイドチャット"].waitUntilExists(timeout: 15), app.debugDescription)
        XCTAssertTrue(app.buttons["workbench.add"].exists)
        XCTAssertTrue(app.navigationBars.buttons["BackButton"].exists)
    }

    func testSimulatorCopiesSelectedOrWholeUserMessageText() throws {
        let app = try connectedSimulatorApp()
        let text = "Needle Alpha Bravo [success]"
        startSimulatorConversation(app, promptText: text)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 30))
        let sent = app.textViews.matching(NSPredicate(
            format: "identifier == %@ AND value == %@", "message.user-text", text
        )).firstMatch
        waitForStableFrame(sent)
        // User messages select in place like answers.
        sent.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        let copy = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Copy", "コピー"])).firstMatch
        XCTAssertTrue(copy.waitUntilExists(timeout: 5)); copy.tap()
        let composer = app.textFields["task.message"]
        composer.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        composer.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitUntilExists(timeout: 5)); paste.tap()
        let selectedText = expectation(for: NSPredicate(format: "value == %@", "Needle"), evaluatedWith: composer)
        wait(for: [selectedText], timeout: 5)
        composer.press(forDuration: 1.2)
        let selectAll = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == 'Select All' OR label == 'すべてを選択'")).firstMatch
        XCTAssertTrue(selectAll.waitUntilExists(timeout: 5)); selectAll.tap()
        composer.typeText(XCUIKeyboardKey.delete.rawValue)
        waitForDraftText(composer, text: composer.placeholderValue ?? "")
        let fullCopy = app.buttons["発言をコピー"]
        XCTAssertTrue(fullCopy.waitUntilExists(timeout: 5)); fullCopy.tap()
        composer.press(forDuration: 1.2)
        XCTAssertTrue(paste.waitUntilExists(timeout: 5)); paste.tap()
        let fullText = expectation(for: NSPredicate(format: "value == %@", text), evaluatedWith: composer)
        wait(for: [fullText], timeout: 5)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "User message copied in place or whole")
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
        XCTAssertTrue(answer.waitUntilExists(timeout: 30))
        let answerID = answer.identifier
        let completed = expectation(
            for: NSPredicate(format: "label CONTAINS %@", "作業しました"),
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
        let row = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(number)")]
        XCTAssertTrue(row.waitUntilExists(timeout: 20), "The chat must be outside collapsed projects")
        row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[answerID].waitUntilExists(timeout: 20))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Unassigned chat after reopening")
        app.buttons["task.new"].tap()
        XCTAssertEqual(app.buttons["task.folder"].label, "フォルダ: チャット")
    }

    func testSimulatorApprovalEditorAndDraftSurviveReconnect() throws {
        let app = try connectedSimulatorApp()
        startSimulatorConversation(app, promptText: "[approval] Verify approvals")
        let accept = app.buttons["request.accept"]
        XCTAssertTrue(accept.waitUntilExists(timeout: 15))
        accept.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 15))

        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Keep this draft")
        openFiles(app)
        let file = app.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitUntilExists(timeout: 10)); file.tap()
        let editor = app.textViews["file.editor"]
        XCTAssertTrue(editor.waitUntilExists(timeout: 10))
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
        XCTAssertTrue(editor.waitUntilExists(timeout: 10))
        XCTAssertTrue((editor.value as? String ?? "").contains("Saved from iPhone"))
        app.buttons["file.close"].tap()
        openWorkbenchTool(app, "diff")
        let savedDiff = app.staticTexts
            .matching(NSPredicate(format: "label BEGINSWITH '+' AND label CONTAINS 'Saved from iPhone'")).firstMatch
        XCTAssertTrue(savedDiff.waitUntilExists(timeout: 10))
        closeWorkbench(app)
        XCTAssertEqual(message.value as? String, "Keep this draft")

        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.bex:")
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        let restored = reopened.descendants(matching: .any)["task.message"]
        XCTAssertTrue(restored.waitUntilExists(timeout: 10))
        XCTAssertEqual(restored.value as? String, "Keep this draft")
    }
}
