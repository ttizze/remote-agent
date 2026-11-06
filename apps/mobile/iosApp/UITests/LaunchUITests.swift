import Foundation
import XCTest

extension XCUIElement {
    func waitUntilExists(timeout: TimeInterval) -> Bool {
        // Avoid XCTest's initial polling delay for elements that already exist.
        exists || waitForExistence(timeout: timeout)
    }
}

final class LaunchUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func captureScreen(_ app: XCUIApplication, named name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func assertComposerAtBottomWithKeyboardHidden(in app: XCUIApplication,
                                                  file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5), file: file, line: line)
        let send = app.buttons["task.send"]
        XCTAssertTrue(send.waitUntilExists(timeout: 10), file: file, line: line)
        waitForStableFrame(send)
        captureScreen(app, named: "Composer position after reopening without keyboard")
        // Allow the home indicator and the composer's own inset, never a keyboard-sized empty region.
        XCTAssertLessThan(app.frame.maxY - send.frame.maxY, 90, file: file, line: line)
    }

    func waitForStableFrame(_ element: XCUIElement) {
        // Sample early and keep failed-poll diagnostics off the accessibility tree.
        var previous = element.frame
        let settled = expectation(for: NSPredicate { _, _ in
            let frame = element.frame
            defer { previous = frame }
            return !frame.isEmpty && frame == previous
        }, evaluatedWith: nil)
        wait(for: [settled], timeout: 10)
    }

    func tapWhenStable(_ element: XCUIElement) {
        XCTAssertTrue(element.waitUntilExists(timeout: 10))
        waitForStableFrame(element)
        element.tap()
    }

    func closeSearch(_ app: XCUIApplication) {
        app.buttons.matching(NSPredicate(
            format: "identifier == %@ OR label IN %@", "close", ["close", "Close", "閉じる"]
        )).firstMatch.tap()
    }

    /// The status bar is the native way back to the start of a conversation.
    func tapStatusBar(_ app: XCUIApplication) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.18, dy: 0)).withOffset(CGVector(dx: 0, dy: 32)).tap()
    }

    func waitForDraftText(_ input: XCUIElement, text: String, timeout: TimeInterval = 15) {
        let ready = expectation(
            for: NSPredicate(format: "exists == true AND value == %@", text), evaluatedWith: input
        )
        wait(for: [ready], timeout: timeout)
    }

    func expandSimulatorProject(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) {
        let project = app.buttons["tasks.project.simulator-project"]
        guard project.waitUntilExists(timeout: 10) else {
            XCTFail("The fixture project did not appear.\n\(app.debugDescription)", file: file, line: line)
            return
        }
        if project.value as? String == "閉じています" {
            project.tap()
        }
    }

    @discardableResult
    func simulatorFixture(
        _ path: String,
        method: String = "POST",
        expectedStatus: Int = 204,
        timeout: TimeInterval = 10,
        file: StaticString = #filePath,
        line: UInt = #line
    ) throws -> Data {
        let pairingURL =
            try XCTUnwrap(try URL(string: XCTUnwrap(ProcessInfo.processInfo.environment["AGENT_PAIRING_URL"])))
        var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent(path))
        request.httpMethod = method
        request.timeoutInterval = timeout
        let completed = expectation(description: "Fixture \(method) \(path)")
        var body = Data()
        URLSession.shared.dataTask(with: request) { data, response, error in
            XCTAssertNil(error, file: file, line: line)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, expectedStatus, file: file, line: line)
            if let data {
                body = data
            }
            completed.fulfill()
        }.resume()
        wait(for: [completed], timeout: timeout)
        return body
    }

    func useSimulatorListFixture(_ path: String) throws {
        addTeardownBlock { _ = try self.simulatorFixture("list-fixture/reset") }
        try simulatorFixture(path)
    }

    func useSimulatorConversationFixture(_ kind: String) throws -> String {
        // The Host deliberately retains imported transcripts. Each test owns a new conversation,
        // while reopening it within the same test must retain that test's submitted turns.
        let id = "fixture-history-\(UUID().uuidString)"
        try useSimulatorListFixture("\(kind)/\(id)")
        return id
    }

    func connectedSimulatorApp(expandProject: Bool = true) throws -> XCUIApplication {
        var app = XCUIApplication()
        if app.state != .notRunning {
            app.terminate()
            if !app.wait(for: .notRunning, timeout: 10) {
                // A Simulator relaunch can replace the process XCTest just stopped.
                // Refresh the application handle before establishing test isolation.
                app = XCUIApplication()
                app.terminate()
            }
            XCTAssertTrue(app.wait(for: .notRunning, timeout: 10))
        }
        app.launch()
        defer {
            if expandProject {
                expandSimulatorProject(app)
            }
        }

        let taskList = app.descendants(matching: .any)["tasks.list"]
        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let manualPairing = app.buttons["pairing.manual"]
        let ready = expectation(for: NSPredicate { _, _ in
            taskDetail.exists || manualPairing.exists ||
                (taskList.exists && (app.buttons["tasks.connect"].exists || app.buttons["tasks.connections"].exists))
        }, evaluatedWith: app)
        wait(for: [ready], timeout: 30)
        if taskDetail.exists {
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }
        if app.buttons["tasks.connect"].exists {
            app.buttons["tasks.connect"].tap()
            XCTAssertTrue(manualPairing.waitUntilExists(timeout: 10))
        }
        if manualPairing.exists {
            let payload = try simulatorPairingPayload()
            for _ in 0 ..< 5 where !manualPairing.isHittable {
                app.swipeUp()
            }
            submitManualPairing(app, payload: payload)
        }

        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskList.waitUntilExists(timeout: 30),
            "Task list did not appear after connecting; notice: \(notice.exists ? notice.label : "(none)")"
        )
        return app
    }

    func confirmPairing(_ app: XCUIApplication) {
        let confirm = app.buttons["pairing.confirm"]
        XCTAssertTrue(confirm.waitUntilExists(timeout: 10))
        XCTAssertTrue(app.staticTexts["pairing.host"].exists)
        XCTAssertEqual(app.staticTexts["pairing.host"].label, "検証 Host")
        XCTAssertTrue(app.staticTexts["pairing.recipients"].exists)
        XCTAssertFalse(app.buttons["pairing.scan"].exists)
        XCTAssertFalse(app.buttons["tasks.new.chat"].isHittable)
        captureScreen(app, named: "Connection disclosure before consent")
        for _ in 0 ..< 4 where !confirm.isHittable {
            app.swipeUp()
        }
        confirm.tap()
    }

    func simulatorTaskRowIdentifier(_ path: String) throws -> String {
        struct Session: Decodable {
            let provider: String
            let id: String
        }

        struct TaskIdentity: Decodable {
            let threadId: Session
        }

        let response = try simulatorFixture(path, expectedStatus: 200, timeout: 15)
        let session = try JSONDecoder().decode(TaskIdentity.self, from: response).threadId
        return "tasks.row.\(session.provider):\(session.id)"
    }

    func startSimulatorConversation(_ app: XCUIApplication, promptText: String) {
        let projectCompose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(
            projectCompose.waitUntilExists(timeout: 10),
            "No project creation button appeared: \(app.debugDescription)"
        )
        projectCompose.tap()
        submitSimulatorConversation(app, promptText: promptText)
    }

    func submitSimulatorConversation(_ app: XCUIApplication, promptText: String) {
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(prompt.waitUntilExists(timeout: 10))
        expandComposer(app)
        // Model-picker interaction has its own native journey. Other tests only
        // need the fixture's default Codex model, regardless of a prior choice.
        if app.buttons["model.settings"].value as? String != "Fixture Model" {
            chooseFixtureModel(app)
        }
        // New drafts focus the field themselves; a picker round trip can dismiss it.
        if !app.keyboards.firstMatch.exists {
            prompt.tap()
        }
        prompt.typeText(promptText)

        let start = app.buttons["task.send"]
        XCTAssertTrue(start.waitUntilExists(timeout: 10))
        let startEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: start)
        wait(for: [startEnabled], timeout: 10)
        start.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskDetail.waitUntilExists(timeout: 30),
            "Conversation did not open; notice: \(notice.exists ? notice.label : "(none)")"
        )
        waitForStableFrame(prompt)
        XCTAssertTrue(prompt.isHittable)
    }

    func prefixedElement(_ app: XCUIApplication, prefix: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
            .firstMatch
    }

    func prefixedButton(_ app: XCUIApplication, prefix: String) -> XCUIElement {
        app.buttons
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
            .firstMatch
    }

    func waitForPrefixedElement(
        _ app: XCUIApplication,
        prefix: String,
        scrolling scrollView: XCUIElement
    ) -> Bool {
        let element = prefixedElement(app, prefix: prefix)
        if element.waitUntilExists(timeout: 1) {
            return true
        }
        for _ in 0 ..< 14 {
            scrollView.swipeUp()
            if element.waitUntilExists(timeout: 1) {
                return true
            }
        }
        return false
    }

    func allowFirstSystemPermissionIfPresent() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let alert = springboard.alerts.firstMatch
        guard alert.waitUntilExists(timeout: 3) else { return }

        let positiveLabels = [
            "許可",
            "Allow",
            "Allow While Using App",
            "Allow Once",
            "許可する",
            "一度だけ許可"
        ]
        for label in positiveLabels {
            let button = alert.buttons[label]
            if button.exists {
                button.tap()
                return
            }
        }
    }

    func simulatorPairingPayload() throws -> String {
        let data = try simulatorFixture("pairing", method: "GET", expectedStatus: 200, timeout: 30)
        let payload = try XCTUnwrap(String(data: data, encoding: .utf8))
        guard !payload.isEmpty else { throw PairingPayloadError.invalidResponse }
        // XCTest typing can drop characters unavailable on the active keyboard.
        // JSON escapes preserve the exact invitation, including the PC name.
        return payload.utf16.map { unit in
            unit > 127 ? String(format: "\\u%04x", unit) : String(UnicodeScalar(unit)!)
        }.joined()
    }
}

private enum PairingPayloadError: Error {
    case invalidResponse
}

extension LaunchUITests {
    func openFiles(_ app: XCUIApplication) {
        app.buttons["task.tools"].tap()
        openWorkbenchTool(app, "files")
    }

    func workbenchTab(_ app: XCUIApplication, _ kind: String) -> XCUIElement {
        // UIKit's title menu keeps the native labels and selection traits, but drops custom identifiers.
        let titles = ["terminal": "ターミナル", "files": "ファイル", "browser": "ブラウザ",
                      "diff": "差分", "sideChat": "サイドチャット", "agents": "エージェント"]
        let tabs = app.collectionViews.buttons.matching(NSPredicate(
            format: "label == %@ AND identifier == ''", titles[kind] ?? kind
        ))
        return tabs.element(boundBy: max(0, tabs.count - 1))
    }

    /// Open tools are listed in the navigation title's menu, which the bar labels with its title.
    func openWorkbenchTabs(_ app: XCUIApplication) {
        let bar = app.navigationBars.firstMatch
        XCTAssertTrue(bar.waitUntilExists(timeout: 10))
        tapWhenStable(bar.buttons[bar.identifier])
    }

    func dismissWorkbenchMenu(_ app: XCUIApplication) {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
    }

    func openWorkbenchTool(_ app: XCUIApplication, _ kind: String) {
        let tool = app.buttons["workbench.\(kind)"]
        if app.descendants(matching: .any)["workbench.launcher"].exists {
            tool.tap(); return
        }
        openWorkbenchTabs(app)
        let tab = workbenchTab(app, kind)
        if tab.exists {
            tab.tap(); return
        }
        dismissWorkbenchMenu(app)
        app.buttons["workbench.add"].tap()
        XCTAssertTrue(tool.waitUntilExists(timeout: 5)); tool.tap()
    }

    func closeWorkbench(_ app: XCUIApplication) {
        for _ in 0 ..< 10 where !app.buttons["task.tools"].exists {
            tapWhenStable(app.navigationBars.buttons["BackButton"].firstMatch)
        }
        XCTAssertTrue(app.buttons["task.tools"].waitUntilExists(timeout: 5))
        XCTAssertTrue(app.textFields["task.message"].exists)
    }
}
