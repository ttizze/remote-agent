import Foundation
import UIKit
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    private func assertTerminalReady(_ app: XCUIApplication) {
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        let ready = expectation(for: NSPredicate(format: "value == %@", "入力可能"), evaluatedWith: terminal)
        wait(for: [ready], timeout: 10)
        XCTAssertFalse(app.staticTexts["実行中"].exists)
        XCTAssertFalse(app.staticTexts["起動中"].exists)
        XCTAssertFalse(app.progressIndicators["terminal.loading"].exists)
    }

    func testSimulatorWorkbenchStartsEmptyAndOwnsOnlyOpenedTabs() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["task.tools"].tap()
        let launcher = app.descendants(matching: .any)["workbench.launcher"]
        XCTAssertTrue(launcher.waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["workbench.add"].exists)
        XCTAssertTrue(app.navigationBars["ツール"].exists)
        XCTAssertTrue(app.navigationBars.buttons["BackButton"].exists)
        XCTAssertFalse(app.buttons["このツールを閉じる"].exists)
        XCTAssertFalse(workbenchTab(app, "terminal").exists)
        XCTAssertFalse(app.descendants(matching: .any)["terminal.screen"].exists)
        openWorkbenchTool(app, "terminal")
        assertTerminalReady(app)
        XCTAssertTrue(app.navigationBars["ターミナル"].exists)
        XCTAssertFalse(launcher.exists)
        openWorkbenchTool(app, "files")
        XCTAssertTrue(app.buttons["file.hello.txt"].waitUntilExists(timeout: 10))
        openWorkbenchTabs(app)
        let terminalTab = workbenchTab(app, "terminal")
        let filesTab = workbenchTab(app, "files")
        XCTAssertTrue(terminalTab.exists, app.debugDescription)
        XCTAssertTrue(filesTab.exists, app.debugDescription)
        XCTAssertTrue(filesTab.isSelected)
        app.buttons["このツールを閉じる"].tap()
        assertTerminalReady(app)
        openWorkbenchTabs(app)
        XCTAssertFalse(filesTab.exists)
        app.buttons["このツールを閉じる"].tap()
        XCTAssertTrue(launcher.waitUntilExists(timeout: 5))
        captureScreen(app, named: "Native tool list with back navigation")
        closeWorkbench(app)
    }

    func testSimulatorBrowserUsesOneSharedImportMenu() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Shared browser import menu")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 30))
        app.buttons["task.tools"].tap()
        openWorkbenchTool(app, "browser")
        let menu = app.buttons["browser.menu"]
        XCTAssertTrue(menu.waitUntilExists(timeout: 20))
        // A cold Host launch has a 20-second deadline, followed by page setup or a reconnect.
        wait(for: [expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: menu)], timeout: 60)
        let keyboard = app.buttons["browser.keyboard"]
        XCTAssertTrue(keyboard.exists)
        keyboard.tap()
        let input = app.secureTextFields["browser.text"]
        XCTAssertTrue(input.waitUntilExists(timeout: 5))
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        input.typeText("mobile input")
        XCTAssertTrue(app.buttons["browser.type"].isEnabled)
        app.buttons["browser.keyboard.dismiss"].tap()
        XCTAssertFalse(input.exists)
        XCTAssertFalse(app.buttons["個人用"].exists)
        menu.tap()
        app.buttons["browser.credentials"].tap()
        let data = app.navigationBars["ブラウザデータ"]
        XCTAssertTrue(data.waitUntilExists(timeout: 20))
        XCTAssertTrue(app.staticTexts["接続先のブラウザから取り込む"].exists)
        captureScreen(app, named: "One shared browser with import menu")
        data.buttons["閉じる"].tap()
        XCTAssertTrue(menu.waitUntilExists(timeout: 5))
        closeWorkbench(app)
    }

    func testSimulatorNativeTerminalCopiesTextOnLongPress() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["task.tools"].tap()
        openWorkbenchTool(app, "terminal")
        assertTerminalReady(app)
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        terminal.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        terminal.typeText("printf '\\033[2J\\033[HNeedle Alpha Bravo\\n'; exit 0\n")
        let status = app.staticTexts["終了 · 0"]
        XCTAssertTrue(status.waitUntilExists(timeout: 10))
        waitForStableFrame(terminal)
        // Anchor below the status text, which shifts the terminal when the shell exits.
        let word = app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(
            dx: terminal.frame.minX + 20 - app.frame.minX,
            dy: status.frame.maxY + 8 - app.frame.minY
        ))
        word.press(forDuration: 1.2)
        let copy = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Copy", "コピー"])).firstMatch
        XCTAssertTrue(copy.waitUntilExists(timeout: 5))
        captureScreen(app, named: "Terminal long press selects a word and exposes Copy")
        // A second long press on Bravo reopens the menu without changing Needle's selection.
        word.withOffset(CGVector(dx: 100, dy: 0)).press(forDuration: 1.2)
        XCTAssertTrue(copy.waitUntilExists(timeout: 5)); copy.tap()
        closeWorkbench(app)
        let composer = app.textFields["task.message"]
        // Project drafts survive earlier tests and app relaunches.
        replaceFieldText(composer, text: "")
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        composer.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitUntilExists(timeout: 5)); paste.tap()
        waitForDraftText(composer, text: "Needle")
        captureScreen(app, named: "Terminal long press preserves and copies only the selected word")
    }

    func testSimulatorNativeTerminalPastesMultilineText() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["task.tools"].tap()
        openWorkbenchTool(app, "terminal")
        assertTerminalReady(app)
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitUntilExists(timeout: 5))
        terminal.tap()
        // The keyboard resizes the terminal; locate the long-press target after it appears.
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        UIPasteboard.general.string = "AGENT_PASTE='日本語\nsecond line'"
        terminal.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitUntilExists(timeout: 5)); paste.tap()
        terminal.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitUntilExists(timeout: 5))
        terminal.typeText("\n")
        let expected = "\\346\\227\\245\\346\\234\\254\\350\\252\\236\\nsecond line"
        terminal.typeText("[ \"$AGENT_PASTE\" = \"$(printf '\(expected)')\" ]; exit $?\n")
        XCTAssertTrue(app.staticTexts["終了 · 0"].waitUntilExists(timeout: 15))
        captureScreen(app, named: "Native terminal pastes Unicode and multiline text")
        closeWorkbench(app)
    }

    func testSimulatorNativeTerminalDoesNotDuplicateQueryResponses() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["task.tools"].tap()
        openWorkbenchTool(app, "terminal")
        assertTerminalReady(app)
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitUntilExists(timeout: 5))
        terminal.tap()
        let probe = try XCTUnwrap(ProcessInfo.processInfo.environment["AGENT_TERMINAL_QUERY_PROBE"])
            .replacingOccurrences(of: "'", with: "'\\''")
        terminal.typeText("'\(probe)' terminal-query-probe; exit $?\n")
        XCTAssertTrue(app.staticTexts["終了 · 0"].waitUntilExists(timeout: 20))
        captureScreen(app, named: "Only the Host responds to terminal queries")
        closeWorkbench(app)
    }

    func testSimulatorNativeTerminalRetainsShellAfterReopening() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        func openTerminal() {
            app.buttons["task.tools"].tap()
            openWorkbenchTool(app, "terminal")
            assertTerminalReady(app)
        }
        let composer = app.textFields["task.message"]
        replaceFieldText(composer, text: "Keep my draft")
        let chat = app.descendants(matching: .any)["task.empty"]
        XCTAssertTrue(chat.waitUntilExists(timeout: 10))
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0))
            .withOffset(CGVector(dx: 0, dy: chat.frame.midY - app.frame.minY))
        edge.press(forDuration: 0.05, thenDragTo: edge.withOffset(CGVector(dx: -200, dy: 0)))
        XCTAssertTrue(app.descendants(matching: .any)["workbench.launcher"].waitUntilExists(timeout: 5))
        openWorkbenchTool(app, "terminal")
        assertTerminalReady(app)
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitUntilExists(timeout: 5))
        terminal.tap(); terminal.typeText("AGENT_NATIVE=17\n")
        captureScreen(app, named: "Native terminal with keyboard")
        XCTAssertFalse(app.buttons["終了"].exists)
        XCTAssertFalse(app.buttons["terminal.close"].exists)
        openWorkbenchTool(app, "files")
        let file = app.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitUntilExists(timeout: 10))
        captureScreen(app, named: "Full-screen files")
        file.tap()
        XCTAssertTrue(app.textViews["file.editor"].waitUntilExists(timeout: 10))
        app.buttons["file.close"].tap()
        openWorkbenchTool(app, "terminal")
        assertTerminalReady(app)
        XCTAssertEqual(terminal.frame.width, app.frame.width, accuracy: 2)
        let back = app.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: 1, dy: app.navigationBars.firstMatch.frame.maxY + 40 - app.frame.minY))
        back.press(forDuration: 0.1, thenDragTo: back.withOffset(CGVector(dx: terminal.frame.width * 0.9, dy: 0)),
                   withVelocity: .fast, thenHoldForDuration: 0)
        XCTAssertTrue(app.buttons["task.tools"].waitUntilExists(timeout: 5))
        XCTAssertFalse(workbenchTab(app, "terminal").exists)
        XCTAssertFalse(terminal.exists)
        XCTAssertEqual(composer.value as? String, "Keep my draft")
        openTerminal()
        terminal.tap(); terminal.typeText("exit $AGENT_NATIVE\n")
        XCTAssertTrue(app.staticTexts["終了 · 17"].waitUntilExists(timeout: 10))
        captureScreen(app, named: "Reattached native shell retains state")
        closeWorkbench(app)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        verifyUnassignedTools(app)
    }

    private func verifyUnassignedTools(_ app: XCUIApplication) {
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["tasks.new.chat"].tap()
        app.buttons["task.tools"].tap()
        for page in ["terminal", "files"] {
            XCTAssertFalse(app.buttons["workbench.\(page)"].isEnabled)
        }
        closeWorkbench(app)
        XCTAssertFalse(app.staticTexts["notice"].exists)
    }

    func testSimulatorUsesNativeProjectDisclosureAndWorkspaceTree() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Native project disclosure")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitUntilExists(timeout: 25))
        openFiles(app)
        let nested = app.buttons["file.nested"]
        XCTAssertTrue(nested.waitUntilExists(timeout: 10))
        let navigation = app.navigationBars["ファイル"]
        XCTAssertTrue(navigation.exists)
        let rootFile = app.buttons["file.hello.txt"]
        XCTAssertTrue(rootFile.exists)
        captureScreen(app, named: "Native workspace file tree")
        nested.tap()
        let child = app.buttons["file.child.txt"]
        XCTAssertTrue(child.waitUntilExists(timeout: 10))
        XCTAssertTrue(rootFile.exists)
        XCTAssertEqual(app.navigationBars.count, 1)
        captureScreen(app, named: "Expanded workspace folder")
        nested.tap()
        XCTAssertTrue(child.waitForNonExistence(timeout: 10))
        nested.tap()
        XCTAssertTrue(child.waitUntilExists(timeout: 10))
        child.tap()
        let editor = app.textViews["file.editor"]
        XCTAssertTrue(editor.waitUntilExists(timeout: 10))
        let loaded = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value CONTAINS %@", "Nested directory navigation fixture."), object: editor
        )
        XCTAssertEqual(XCTWaiter.wait(for: [loaded], timeout: 10), .completed)
        app.buttons["file.close"].tap()
        XCTAssertTrue(child.waitUntilExists(timeout: 10))
        XCTAssertEqual(app.navigationBars.count, 1)
        app.collectionViews["files.list"].swipeDown()
        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitUntilExists(timeout: 10))
        search.tap(); search.typeText("child\n")
        XCTAssertTrue(child.waitUntilExists(timeout: 10))
        XCTAssertTrue(rootFile.waitForNonExistence(timeout: 10))
        // Focusing the bottom search field moves its clear button above the keyboard.
        search.tap()
        search.buttons.firstMatch.tap()
        XCTAssertTrue(rootFile.waitUntilExists(timeout: 10))
        closeSearch(app)
        closeWorkbench(app)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        XCTAssertTrue(app.textFields["task.message"].waitUntilExists(timeout: 10))
    }

    private func replaceFieldText(_ field: XCUIElement, text: String) {
        field.tap()
        if let value = field.value as? String, !value.isEmpty, value != field.placeholderValue {
            field.typeKey("a", modifierFlags: .command)
            field.typeText(XCUIKeyboardKey.delete.rawValue)
            XCTAssertEqual(field.value as? String, field.placeholderValue)
        }
        if !text.isEmpty {
            field.typeText(text)
        }
    }

    func testPhysicalDeviceCanPairWithManualPayload() throws {
        guard let payload = ProcessInfo.processInfo.environment["AGENT_PAIRING_PAYLOAD"],
              !payload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            throw XCTSkip("AGENT_PAIRING_PAYLOAD is required for the physical-device pairing test")
        }

        let app = XCUIApplication()
        app.launch()
        allowFirstSystemPermissionIfPresent()

        submitManualPairing(app, payload: payload)

        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitUntilExists(timeout: 30), "Task list did not appear after pairing")
        XCTAssertFalse(app.staticTexts["PCとペアリング"].exists)

        let notice = app.staticTexts["notice"]
        let project = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.project."))
            .firstMatch
        if project.exists, project.value as? String == "閉じています" {
            project.tap()
        }
        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(
            firstTask.waitUntilExists(timeout: 30),
            "No task row appeared in the task list; notice: \(notice.exists ? notice.label : "(none)")"
        )
        firstTask.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(
            taskDetail.waitUntilExists(timeout: 30),
            "Task detail content did not appear; notice: \(notice.exists ? notice.label : "(none)")"
        )

        let firstItem = assertLoadedTaskDetails(app)

        app.terminate(); app.launch()

        XCTAssertTrue(taskList.waitUntilExists(timeout: 30), "Relaunch did not open the task list")
        XCTAssertFalse(taskDetail.exists)
        if project.exists, project.value as? String == "閉じています" {
            project.tap()
        }
        XCTAssertTrue(firstTask.waitUntilExists(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitUntilExists(timeout: 30))
        app.navigationBars.buttons.element(boundBy: 0).tap()

        XCTAssertTrue(taskList.waitUntilExists(timeout: 30), "Task list did not reappear after returning")
        XCTAssertTrue(firstTask.waitUntilExists(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitUntilExists(timeout: 30))
        captureScreen(app, named: "Native conversation navigation")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(taskList.waitUntilExists(timeout: 30), "Native edge swipe did not return to the task list")
        XCTAssertFalse(taskDetail.exists)
    }

    func submitManualPairing(_ app: XCUIApplication, payload: String) {
        let manualPairing = app.buttons["pairing.manual"]
        XCTAssertTrue(manualPairing.waitUntilExists(timeout: 10))
        manualPairing.tap()

        let contents = app.secureTextFields["pairing.contents"]
        XCTAssertTrue(contents.waitUntilExists(timeout: 10))
        contents.tap()
        contents.typeText(payload)
        XCTAssertFalse((contents.value as? String ?? "").isEmpty)

        let submit = app.buttons["pairing.submit"]
        XCTAssertTrue(submit.waitUntilExists(timeout: 10))
        let submitEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: submit)
        wait(for: [submitEnabled], timeout: 10)
        submit.tap()
        confirmPairing(app)
    }

    func assertLoadedTaskDetails(_ app: XCUIApplication) -> XCUIElement {
        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let loadingText = app.staticTexts["タスクを読み込み中…"]
        let loadingFinished = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: loadingText
        )
        let loadingResult = XCTWaiter.wait(for: [loadingFinished], timeout: 30)
        XCTAssertTrue(loadingResult == .completed, "Task detail remained on the loading screen")

        let detailMetrics = taskDetail.value as? String ?? "(unavailable)"
        let firstItem = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item."))
            .firstMatch
        XCTAssertTrue(
            firstItem.waitUntilExists(timeout: 30),
            "No rendered task item appeared; detail=\(detailMetrics)"
        )

        let backButton = app.navigationBars.buttons.element(boundBy: 0)
        let composer = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(
            composer.exists && composer.isHittable,
            "Task composer was not visible and hittable after task detail loaded; detail=\(detailMetrics)"
        )
        XCTAssertTrue(
            backButton.exists && backButton.isHittable,
            "Task back button was not visible and hittable after task detail loaded; detail=\(detailMetrics)"
        )
        return firstItem
    }
}
