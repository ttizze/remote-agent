import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
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
        let row = prefixedElement(reopened, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        let restored = reopened.textFields["task.message"]
        XCTAssertTrue(restored.waitForExistence(timeout: 10))
        XCTAssertEqual(restored.value as? String, expected)
    }

    func testSimulatorAsksAboutAssistantSelectionInSideChatAndRestoresOriginalDraft() throws {
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
        let text = original.textViews["message.assistant-text"]
        text.tap()
        text.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 25, dy: 10)).press(forDuration: 1.2)
        if retryRead {
            _ = try simulatorFixture("fail-next-history-read")
        }
        openAssistantSelectionSideChat(app)
        if retryRead {
            let retry = app.buttons["再試行"]
            XCTAssertTrue(retry.waitForExistence(timeout: 10))
            XCTAssertEqual(message.value as? String, "Keep my original draft")
            retry.tap()
        }
        let sheet = app.descendants(matching: .any)["side-chat.sheet"]
        let sideMessage = sheet.textFields["task.message"]
        XCTAssertTrue(sideMessage.waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(sideMessage.value as? String, "> Needle\n\n")
        sideMessage.tap()
        sideMessage.coordinate(withNormalizedOffset: CGVector(dx: 0.95, dy: 0.95)).tap()
        sideMessage.typeText("[success] Explain this selection")
        XCTAssertEqual(
            sideMessage.value as? String, "> Needle\n\n[success] Explain this selection", app.debugDescription
        )
        sheet.buttons["task.send"].tap()
        let final = sheet.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item.fixture-final-")).firstMatch
        XCTAssertTrue(final.waitForExistence(timeout: 30))
        let sideID = final.identifier
        XCTAssertNotEqual(sideID, originalID)
        XCTAssertTrue(sheet.staticTexts["> Needle\n\n[success] Explain this selection"].exists, app.debugDescription)
        let running = sheet.buttons
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "turn.interrupt.")).firstMatch
        let completed = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: running)
        wait(for: [completed], timeout: 10)
        XCTAssertEqual(sideMessage.value as? String, sideMessage.placeholderValue)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Selected quote submitted in side chat")
        try verifySideChatRestored(app, originalID: originalID, sideID: sideID)
    }

    private func openAssistantSelectionSideChat(_ app: XCUIApplication) {
        let side = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "サイドチャットで質問")).firstMatch
        if !side.exists {
            let forward = app.buttons["Forward"]
            XCTAssertTrue(forward.waitForExistence(timeout: 5)); forward.tap()
        }
        XCTAssertTrue(side.waitForExistence(timeout: 5)); side.tap()
        XCTAssertTrue(app.buttons["side-chat.close"].waitForExistence(timeout: 15))
    }

    private func verifySideChatRestored(_ app: XCUIApplication, originalID: String, sideID: String) throws {
        let message = app.textFields["task.message"]
        app.buttons["side-chat.close"].tap()
        XCTAssertTrue(app.descendants(matching: .any)[originalID].waitForExistence(timeout: 15))
        let restored = expectation(
            for: NSPredicate(format: "value == %@", "Keep my original draft"),
            evaluatedWith: message
        )
        wait(for: [restored], timeout: 10)
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let number = try XCTUnwrap(sideID.split(separator: "-").dropLast().last)
        let row = reopened.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
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
        let message = app.textFields["task.message"]
        message.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitForExistence(timeout: 5)); paste.tap()
        XCTAssertEqual(message.value as? String, "Needle")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Only the selected word was copied")
    }

    func testSimulatorCopiesOwnMessageIntoComposer() throws {
        let app = try connectedSimulatorApp()
        let text = "[success] Copy this complete message"
        try startSimulatorConversation(app, promptText: text)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 30))
        app.staticTexts[text].press(forDuration: 1.2)
        let copy = app.buttons["コピー"]
        XCTAssertTrue(copy.waitForExistence(timeout: 10))
        copy.tap()
        let message = app.textFields["task.message"]
        message.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitForExistence(timeout: 5))
        paste.tap()
        XCTAssertEqual(message.value as? String, text)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Own message copied into composer")
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
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
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
        app.buttons["files.close"].tap()
        XCTAssertEqual(message.value as? String, "Keep this draft")

        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.fixture-thread-")
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
    }

    func testSimulatorCanSteerAndStopAnActiveTurn() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] Hold this turn")
        XCTAssertTrue(app.buttons["request.accept"].waitForExistence(timeout: 15))
        captureScreen(app, named: "Active conversation text and tool labels")
        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Change the requested approach")
        app.buttons["task.send"].tap()
        XCTAssertTrue(app.staticTexts["Change the requested approach"].waitForExistence(timeout: 2))
        let stop = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(stop.waitForExistence(timeout: 5)); stop.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch
            .waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["request.accept"].exists)
        XCTAssertFalse(stop.exists)
    }
}
