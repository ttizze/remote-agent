import Foundation
import UIKit
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorNativeTerminalPastesMultilineText() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["task.tools"].tap()
        app.buttons["workbench.terminal"].tap()
        XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitForExistence(timeout: 5))
        terminal.tap()
        UIPasteboard.general.string = "BEX_PASTE='日本語\nsecond line'"
        terminal.press(forDuration: 1.2)
        let paste = app.menuItems.matching(NSPredicate(format: "label IN %@", ["Paste", "ペースト"])).firstMatch
        XCTAssertTrue(paste.waitForExistence(timeout: 5)); paste.tap()
        terminal.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        terminal.typeText("\n")
        let expected = "\\346\\227\\245\\346\\234\\254\\350\\252\\236\\nsecond line"
        terminal.typeText("[ \"$BEX_PASTE\" = \"$(printf '\(expected)')\" ]; exit $?\n")
        XCTAssertTrue(app.staticTexts["終了 · 0"].waitForExistence(timeout: 15))
        captureScreen(app, named: "Native terminal pastes Unicode and multiline text")
        closeWorkbench(app)
    }

    func testSimulatorNativeTerminalDoesNotDuplicateQueryResponses() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        app.buttons["task.tools"].tap()
        app.buttons["workbench.terminal"].tap()
        XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitForExistence(timeout: 5))
        terminal.tap()
        let probe = #"""
        import os,re,select,sys,termios,tty
        saved=termios.tcgetattr(0)
        tty.setraw(0)
        os.write(1,b"\x1b[6n")
        data=os.read(0,128) if select.select([0],[],[],5)[0] else b""
        extra=select.select([0],[],[],1)[0]
        termios.tcsetattr(0,termios.TCSANOW,saved)
        sys.exit(0 if re.fullmatch(rb"\x1b\[[0-9]+;[0-9]+R",data) and not extra else 1)
        """#.split(separator: "\n").joined(separator: "; ")
        terminal.typeText("python3 -c '\(probe)'; exit $?\n")
        XCTAssertTrue(app.staticTexts["終了 · 0"].waitForExistence(timeout: 20))
        captureScreen(app, named: "Only the Host responds to terminal queries")
        closeWorkbench(app)
    }

    func testSimulatorNativeTerminalRetainsShellAfterReopening() throws {
        let app = try connectedSimulatorApp()
        app.buttons["tasks.new.project.simulator-project"].tap()
        func openTerminal() {
            app.buttons["task.tools"].tap()
            let action = app.buttons["workbench.terminal"]
            XCTAssertTrue(action.waitForExistence(timeout: 10)); action.tap()
            XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        }
        let composer = app.textFields["task.message"]
        composer.tap(); composer.typeText("Keep my draft")
        let chat = app.descendants(matching: .any)["task.empty"]
        XCTAssertTrue(chat.waitForExistence(timeout: 10))
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0))
            .withOffset(CGVector(dx: 0, dy: chat.frame.midY - app.frame.minY))
        edge.press(forDuration: 0.05, thenDragTo: edge.withOffset(CGVector(dx: -200, dy: 0)))
        XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        let terminal = app.descendants(matching: .any)["terminal.screen"]
        XCTAssertTrue(terminal.waitForExistence(timeout: 5))
        terminal.tap(); terminal.typeText("BEX_NATIVE=17\n")
        captureScreen(app, named: "Native terminal with keyboard")
        XCTAssertFalse(app.buttons["終了"].exists)
        XCTAssertFalse(app.buttons["terminal.close"].exists)
        app.buttons["workbench.files"].tap()
        let file = app.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitForExistence(timeout: 10))
        captureScreen(app, named: "Full-screen files")
        file.tap()
        XCTAssertTrue(app.textViews["file.editor"].waitForExistence(timeout: 10))
        app.buttons["file.close"].tap()
        app.buttons["workbench.terminal"].tap()
        XCTAssertTrue(app.staticTexts["実行中"].waitForExistence(timeout: 10))
        XCTAssertEqual(terminal.frame.width, app.frame.width, accuracy: 2)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.22))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.22)))
        XCTAssertTrue(app.buttons["task.tools"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["workbench.terminal"].exists)
        XCTAssertFalse(terminal.exists)
        XCTAssertEqual(composer.value as? String, "Keep my draft")
        openTerminal()
        terminal.tap(); terminal.typeText("exit $BEX_NATIVE\n")
        XCTAssertTrue(app.staticTexts["終了 · 17"].waitForExistence(timeout: 10))
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
            app.buttons["workbench.\(page)"].tap()
            XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "フォルダを選択すると"))
                .firstMatch.waitForExistence(timeout: 5))
        }
        closeWorkbench(app)
        XCTAssertFalse(app.staticTexts["notice"].exists)
    }

    func testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Native project disclosure")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        openFiles(app)
        let nested = app.buttons["file.nested"]
        XCTAssertTrue(nested.waitForExistence(timeout: 10)); nested.tap()
        let child = app.buttons["file.child.txt"]
        XCTAssertTrue(child.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["workbench.files"].exists)
        captureScreen(app, named: "Native directory navigation")
        let navigationBar = app.navigationBars["nested"]
        XCTAssertTrue(navigationBar.waitForExistence(timeout: 5))
        let start = navigationBar.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 1))
            .withOffset(CGVector(dx: 0, dy: 100))
        start.press(forDuration: 0.1, thenDragTo: start.withOffset(CGVector(dx: 280, dy: 0)))
        XCTAssertTrue(app.buttons["file.hello.txt"].waitForExistence(timeout: 10))
        XCTAssertFalse(child.exists)
        try verifyAbsoluteDirectoryNavigation(app, child: child)
        let path = app.textFields["絶対パス"]
        replaceFieldText(field: path, text: "relative-path")
        app.buttons["files.open-path"].tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "絶対パスを入力"))
            .firstMatch.waitForExistence(timeout: 10))
        closeWorkbench(app)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    private func verifyAbsoluteDirectoryNavigation(_ app: XCUIApplication, child: XCUIElement) throws {
        let path = app.textFields["絶対パス"]
        let root = try XCTUnwrap(path.value as? String)
        XCTAssertTrue(root.hasPrefix("/"))
        replaceFieldText(field: path, text: root + "/nested")
        for _ in 0 ..< 2 {
            XCTAssertEqual(path.value as? String, root + "/nested")
            app.buttons["files.open-path"].tap()
            XCTAssertTrue(child.waitForExistence(timeout: 10))
            XCTAssertTrue(app.buttons["workbench.files"].exists)
            app.navigationBars["nested"].buttons.element(boundBy: 0).tap()
            XCTAssertTrue(app.buttons["file.hello.txt"].waitForExistence(timeout: 10))
            XCTAssertFalse(child.exists)
        }
    }

    func testSimulatorBrowserIsSeparateFromConversationAndPreservesPage() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "Browser sharing fixture")
        XCTAssertFalse(app.buttons["workbench.browser"].exists)
        app.buttons["task.tools"].tap()
        app.buttons["workbench.browser"].tap()
        let address = app.textFields["browser.address"]
        XCTAssertTrue(address.waitForExistence(timeout: 10))
        let takeover = app.buttons["browser.take"]
        let ready = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: takeover)
        wait(for: [ready], timeout: 20)
        takeover.tap()
        XCTAssertTrue(app.buttons["browser.release"].waitForExistence(timeout: 10))
        replaceFieldText(field: address, text: "file:///etc/passwd")
        app.buttons["browser.open"].tap()
        XCTAssertTrue(app.staticTexts["http または https の URL を入力してください"].waitForExistence(timeout: 5))
        let pairing = try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])
        let url = try XCTUnwrap(URL(string: pairing)).deletingLastPathComponent().appendingPathComponent("browser-test")
        replaceFieldText(field: address, text: url.absoluteString)
        let open = app.buttons["browser.open"]
        let recovered = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: open)
        wait(for: [recovered], timeout: 10)
        open.tap()
        XCTAssertTrue(app.staticTexts["BEX browser fixture"].waitForExistence(timeout: 10))
        let canvas = app.descendants(matching: .any)["browser.content"]
        XCTAssertTrue(canvas.waitForExistence(timeout: 10))
        canvas.coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: canvas.frame.width * 100 / 1024, dy: canvas.frame.width * 40 / 1024)).tap()
        app.buttons["文字入力"].tap()
        let input = app.secureTextFields["browser.text"]
        input.tap(); input.typeText("Remote phone input")
        app.buttons["browser.type"].tap()
        XCTAssertTrue(app.staticTexts["Remote phone input"].waitForExistence(timeout: 10))
        captureScreen(app, named: "Shared Host browser with human control")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(app.buttons["task.tools"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["workbench.browser"].exists)
        app.buttons["task.tools"].tap()
        app.buttons["workbench.browser"].tap()
        XCTAssertTrue(app.buttons["browser.release"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Remote phone input"].waitForExistence(timeout: 10))
        app.buttons["browser.release"].tap()
        XCTAssertTrue(app.buttons["browser.take"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["browser.open"].isEnabled)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(app.buttons["task.tools"].waitForExistence(timeout: 5))
        XCTAssertFalse(address.exists)
    }

    private func replaceFieldText(field: XCUIElement, text: String) {
        field.tap(withNumberOfTaps: 3, numberOfTouches: 1)
        field.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertEqual(field.value as? String, field.placeholderValue)
        field.typeText(text)
    }

    func testPhysicalDeviceCanPairWithManualPayload() throws {
        guard let payload = ProcessInfo.processInfo.environment["BEX_PAIRING_PAYLOAD"],
              !payload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            throw XCTSkip("BEX_PAIRING_PAYLOAD is required for the physical-device pairing test")
        }

        let app = XCUIApplication()
        app.launch()
        allowFirstSystemPermissionIfPresent()

        submitManualPairing(app, payload: payload)

        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not appear after pairing")
        XCTAssertFalse(app.staticTexts["PCとペアリング"].exists)

        let notice = app.staticTexts["notice"]
        let project = prefixedButton(app, prefix: "tasks.project.")
        if project.exists, project.value as? String == "閉じています" {
            project.tap()
        }
        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(
            firstTask.waitForExistence(timeout: 30),
            "No task row appeared in the task list; notice: \(notice.exists ? notice.label : "(none)")"
        )
        firstTask.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Task detail content did not appear; notice: \(notice.exists ? notice.label : "(none)")"
        )

        let firstItem = assertLoadedTaskDetails(app)

        app.terminate(); app.launch()

        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Relaunch did not open the task list")
        XCTAssertFalse(taskDetail.exists)
        if project.exists, project.value as? String == "閉じています" {
            project.tap()
        }
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitForExistence(timeout: 30))
        app.navigationBars.buttons.element(boundBy: 0).tap()

        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not reappear after returning")
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitForExistence(timeout: 30))
        captureScreen(app, named: "Native conversation navigation")
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Native edge swipe did not return to the task list")
        XCTAssertFalse(taskDetail.exists)
    }

    func submitManualPairing(_ app: XCUIApplication, payload: String) {
        let manualPairing = app.buttons["QRの内容を手入力"]
        XCTAssertTrue(manualPairing.waitForExistence(timeout: 10))
        manualPairing.tap()

        let contents = app.secureTextFields["pairing.contents"]
        XCTAssertTrue(contents.waitForExistence(timeout: 10))
        contents.tap()
        contents.typeText(payload)
        XCTAssertFalse((contents.value as? String ?? "").isEmpty)

        let submit = app.buttons["pairing.submit"]
        XCTAssertTrue(submit.waitForExistence(timeout: 10))
        let submitEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: submit)
        wait(for: [submitEnabled], timeout: 10)
        submit.tap()
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
            firstItem.waitForExistence(timeout: 30),
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
