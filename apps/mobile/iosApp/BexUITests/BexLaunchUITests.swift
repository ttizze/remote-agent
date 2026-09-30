import Foundation
import XCTest

final class BexLaunchUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func captureScreen(_ app: XCUIApplication, named name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func openFiles(_ app: XCUIApplication) {
        app.buttons["task.tools"].tap()
        let files = app.buttons["workbench.files"]
        XCTAssertTrue(files.waitForExistence(timeout: 5)); files.tap()
        app.buttons["files.all"].tap()
    }

    func closeWorkbench(_ app: XCUIApplication) {
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 5))
    }

    func expandSimulatorProject(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) {
        let project = app.buttons["tasks.project.simulator-project"]
        guard project.waitForExistence(timeout: 10) else {
            XCTFail("The fixture project did not appear", file: file, line: line)
            return
        }
        if project.value as? String == "閉じています" {
            project.tap()
        }
    }

    @discardableResult
    func simulatorFixture(_ path: String, expectedStatus: Int = 204, timeout: TimeInterval = 10,
                          file: StaticString = #filePath, line: UInt = #line) throws -> Data {
        let pairingURL =
            try XCTUnwrap(try URL(string: XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent(path))
        request.httpMethod = "POST"
        request.timeoutInterval = timeout
        let completed = expectation(description: "Fixture POST \(path)")
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

    func connectedSimulatorApp(expandProject: Bool = true) throws -> XCUIApplication {
        let app = XCUIApplication()
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
            taskList.exists || taskDetail.exists || manualPairing.exists
        }, evaluatedWith: app)
        wait(for: [ready], timeout: 30)
        if taskDetail.exists {
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }
        if manualPairing.exists {
            let payload = try simulatorPairingPayload()
            for _ in 0 ..< 5 where !manualPairing.isHittable {
                app.swipeUp()
            }
            manualPairing.tap()
            let contents = app.secureTextFields["pairing.contents"]
            XCTAssertTrue(contents.waitForExistence(timeout: 10))
            contents.tap()
            contents.typeText(payload)
            let submit = app.buttons["pairing.submit"]
            XCTAssertTrue(submit.waitForExistence(timeout: 10))
            let submitEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: submit)
            wait(for: [submitEnabled], timeout: 10)
            submit.tap()
            XCTAssertTrue(app.staticTexts["pairing.host"].waitForExistence(timeout: 10))
            XCTAssertEqual(app.staticTexts["pairing.host"].label, "検証 Host")
            confirmPairing(app)
        }

        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskList.waitForExistence(timeout: 30),
            "Task list did not appear after connecting; notice: \(notice.exists ? notice.label : "(none)")"
        )
        return app
    }

    func confirmPairing(_ app: XCUIApplication) {
        let confirm = app.buttons["pairing.confirm"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["pairing.host"].exists)
        XCTAssertFalse(app.staticTexts["pairing.host"].label.isEmpty)
        XCTAssertTrue(app.staticTexts["pairing.recipients"].exists)
        XCTAssertFalse(app.buttons["pairing.scan"].exists)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.list"].exists)
        captureScreen(app, named: "Connection disclosure before consent")
        for _ in 0 ..< 4 where !confirm.isHittable {
            app.swipeUp()
        }
        confirm.tap()
    }

    func startSimulatorConversation(_ app: XCUIApplication, promptText: String) throws {
        let projectCompose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(
            projectCompose.waitForExistence(timeout: 10),
            "No project creation button appeared: \(app.debugDescription)"
        )
        projectCompose.tap()

        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap()
        prompt.typeText(promptText)

        let start = app.buttons["task.send"]
        XCTAssertTrue(start.waitForExistence(timeout: 10))
        let startEnabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: start)
        wait(for: [startEnabled], timeout: 10)
        start.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Conversation did not open; notice: \(notice.exists ? notice.label : "(none)")"
        )
        XCTAssertTrue(app.descendants(matching: .any)["task.message"].isHittable)
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
        if element.waitForExistence(timeout: 1) {
            return true
        }
        for _ in 0 ..< 14 {
            scrollView.swipeUp()
            if element.waitForExistence(timeout: 1) {
                return true
            }
        }
        return false
    }

    func allowFirstSystemPermissionIfPresent() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let alert = springboard.alerts.firstMatch
        guard alert.waitForExistence(timeout: 3) else { return }

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
        let url = try XCTUnwrap(try URL(string: XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        let data = try Data(contentsOf: url)
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
