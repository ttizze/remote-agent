import Foundation
import XCTest

final class BexLaunchUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func testLaunchKeepsPairingScreenAlive() {
        let app = XCUIApplication()
        app.launch()

        let pairingTitle = app.staticTexts["PCとペアリング"]
        XCTAssertTrue(pairingTitle.waitForExistence(timeout: 10))

        let stayedAlive = XCTWaiter.wait(
            for: [XCTestExpectation(description: "observe process stability")],
            timeout: 2
        )
        XCTAssertEqual(stayedAlive, .timedOut)
        XCTAssertEqual(app.state, .runningForeground)
        XCTAssertTrue(pairingTitle.exists)
    }

    func testSimulatorDictationPermissionDenialPreservesDraftAndSend() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap(); prompt.typeText("Keep this draft after microphone denial")
        let microphone = app.buttons["dictation.toggle"]
        XCTAssertTrue(microphone.exists); microphone.tap()
        let systemAlert = XCUIApplication(bundleIdentifier: "com.apple.springboard").alerts.firstMatch
        if systemAlert.waitForExistence(timeout: 5) {
            let deny = systemAlert.buttons.matching(NSPredicate(format: "label IN %@", ["Don't Allow", "Don’t Allow", "許可しない"])).firstMatch
            XCTAssertTrue(deny.exists, "The microphone permission dialog has no recognized deny action")
            deny.tap()
        }
        let notice = app.staticTexts["notice"]
        XCTAssertTrue(notice.waitForExistence(timeout: 10))
        XCTAssertTrue(notice.label.contains("マイク"))
        XCTAssertEqual(prompt.value as? String, "Keep this draft after microphone denial")
        XCTAssertFalse(app.staticTexts["dictation.recording"].exists)
        XCTAssertFalse(app.buttons["dictation.cancel"].exists)
        XCTAssertTrue(microphone.isEnabled)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Dictation permission denied with draft retained"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
    }

    func testSimulatorOpensListAndKeepsModelAfterRelaunchAndForeground() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let projectCompose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(projectCompose.waitForExistence(timeout: 10)); projectCompose.tap()
        app.buttons["model.settings"].tap()
        XCTAssertFalse(app.buttons["model.picker"].exists)
        app.buttons["model.details"].tap()
        let model = app.buttons["model.picker"]
        XCTAssertTrue(model.waitForExistence(timeout: 10)); model.tap()
        let choice = app.buttons["Fixture Model"]
        XCTAssertTrue(choice.waitForExistence(timeout: 10)); choice.tap()
        let effort = app.buttons["model.effort"]
        XCTAssertTrue(effort.waitForExistence(timeout: 10)); effort.tap()
        app.buttons["high"].tap()
        app.buttons["model.close"].tap()
        let prompt = app.descendants(matching: .any)["task.message"]
        prompt.tap(); prompt.typeText("[model] Verify chosen settings")
        app.buttons["task.send"].tap()
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 25), "Chosen settings were rejected by the fixture")
        let finalID = finalAnswer.identifier
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.exists)
        app.terminate(); app.launch()
        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30))
        XCTAssertFalse(detail.exists)
        let threadNumber = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(threadNumber)"]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitForExistence(timeout: 30))
        let settings = app.buttons["model.settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["model.picker"].exists)
        XCTAssertGreaterThan(settings.frame.midX, app.frame.midX)
        XCTAssertGreaterThan(settings.frame.midY, app.descendants(matching: .any)["task.message"].frame.midY)
        settings.tap()
        XCTAssertFalse(app.buttons["model.picker"].exists)
        XCTAssertTrue(app.buttons["model.details"].label.contains("Fixture Model"))
        app.segmentedControls["model.quick.effort"].buttons["medium"].tap()
        XCTAssertTrue(app.buttons["model.details"].label.contains("medium"))
        app.segmentedControls["model.quick.effort"].buttons["high"].tap()
        let quickScreenshot = XCTAttachment(screenshot: app.screenshot())
        quickScreenshot.name = "Quick model settings"; quickScreenshot.lifetime = .keepAlways; add(quickScreenshot)
        app.buttons["model.details"].tap()
        XCTAssertTrue(app.buttons["model.picker"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["model.picker"].label.contains("Fixture Model"))
        XCTAssertTrue(app.buttons["model.effort"].label.contains("high"))
        app.buttons["model.effort"].tap(); app.buttons["medium"].tap()
        app.buttons["model.effort"].tap(); app.buttons["high"].tap()
        app.buttons["model.close"].tap()
        XCUIDevice.shared.press(.home); app.activate()
        XCTAssertTrue(taskList.waitForExistence(timeout: 15))
        XCTAssertFalse(detail.exists)
        let listScreenshot = XCTAttachment(screenshot: app.screenshot())
        listScreenshot.name = "Task list on foreground return"; listScreenshot.lifetime = .keepAlways; add(listScreenshot)
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitForExistence(timeout: 20))
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10)); composer.tap()
        composer.typeText("[model] Verify settings after restart")
        let send = app.buttons["task.send"]
        let ready = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        wait(for: [ready], timeout: 30); send.tap()
        let nextAnswer = app.descendants(matching: .any)[String(finalID.dropLast()) + "2"]
        XCTAssertTrue(nextAnswer.waitForExistence(timeout: 25))
        composer.tap()
        XCTAssertTrue(settings.isHittable)
        let screenshot = XCTAttachment(screenshot: app.screenshot()); screenshot.name = "Restored task and selected model"; screenshot.lifetime = .keepAlways; add(screenshot)
    }

    func testSimulatorReturnsToListWithNativeEdgeSwipeAndRetainsDrafts() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let message = app.textFields["task.message"]
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        message.tap(); message.typeText("[success] Retain this project draft")
        func swipeBack() {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
                .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
            XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
            XCTAssertFalse(message.exists)
        }
        swipeBack()
        compose.tap()
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertEqual(message.value as? String, "[success] Retain this project draft")
        app.buttons["task.send"].tap()
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let answerID = answer.identifier
        let number = try XCTUnwrap(answerID.split(separator: "-").dropLast().last)
        message.tap(); message.typeText("Keep this conversation draft")
        swipeBack()
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[answerID].waitForExistence(timeout: 15))
        XCTAssertEqual(message.value as? String, "Keep this conversation draft")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Native navigation after edge swipe with retained draft"
        screenshot.lifetime = .keepAlways; add(screenshot)
        swipeBack()
    }

    func testSimulatorUsesNativeHostNavigationAndPairingDismissal() throws {
        let app = try connectedSimulatorApp()
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let addHost = app.buttons["profiles.add"]
        XCTAssertTrue(addHost.waitForExistence(timeout: 10)); addHost.tap()
        let cancel = app.buttons["pairing.cancel"]
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.13))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)))
        XCTAssertTrue(addHost.waitForExistence(timeout: 10))
        XCTAssertFalse(cancel.exists)
        addHost.tap()
        XCTAssertTrue(cancel.waitForExistence(timeout: 10)); cancel.tap()
        let host = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND identifier != %@", "profiles.", "profiles.add")).firstMatch
        XCTAssertTrue(host.waitForExistence(timeout: 10)); host.tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
    }

    func testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject() throws {
        let app = try connectedSimulatorApp()
        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        XCTAssertGreaterThan(search.frame.midY, app.frame.height * 0.8)
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.exists)
        project.tap()
        XCTAssertEqual(project.value as? String, "閉じています")
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.isHittable)
        let collapsed = XCTAttachment(screenshot: app.screenshot())
        collapsed.name = "Remote projects with bottom search"; collapsed.lifetime = .keepAlways; add(collapsed)
        compose.tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["tasks.new.chat"].exists)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        search.tap(); search.typeText("no-matching-conversation-unique\n")
        XCTAssertFalse(app.buttons["tasks.project.simulator-project"].exists)
        search.buttons.firstMatch.tap()
        app.buttons["close"].tap()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(project.waitForExistence(timeout: 10))
        project.tap()
        let expanded = XCTAttachment(screenshot: app.screenshot())
        expanded.name = "Expanded project rows"; expanded.lifetime = .keepAlways; add(expanded)
        app.buttons["tasks.new.chat"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    func testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Native project disclosure")
        let answer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(answer.waitForExistence(timeout: 25))
        let number = try XCTUnwrap(answer.identifier.split(separator: "-").dropLast().last)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(row.waitForExistence(timeout: 15))
        let project = app.buttons["tasks.project.simulator-project"]
        XCTAssertTrue(project.waitForExistence(timeout: 10)); project.tap()
        XCTAssertFalse(row.exists)
        project.tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        openFiles(app)
        let nested = app.buttons["file.nested"]
        XCTAssertTrue(nested.waitForExistence(timeout: 10)); nested.tap()
        let child = app.buttons["file.child.txt"]
        XCTAssertTrue(child.waitForExistence(timeout: 10))
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Native directory navigation"
        screenshot.lifetime = .keepAlways; add(screenshot)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(app.buttons["file.hello.txt"].waitForExistence(timeout: 10))
        XCTAssertFalse(child.exists)
        app.buttons["files.close"].tap()
        XCTAssertTrue(app.textFields["task.message"].waitForExistence(timeout: 10))
    }

    func testSimulatorUpdatesAnOpenConversationFromAnotherClient() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        func updateFixture(_ path: String) {
            var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent(path))
            request.httpMethod = "POST"
            let updated = expectation(description: "Other Codex process changed persisted history")
            URLSession.shared.dataTask(with: request) { _, response, error in
                XCTAssertNil(error)
                XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
                updated.fulfill()
            }.resume()
            wait(for: [updated], timeout: 10)
        }
        updateFixture("external-conversation")
        defer { updateFixture("list-fixture/reset") }
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let row = app.descendants(matching: .any)["tasks.row.fixture-external-thread"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-fixture-external-thread"].waitForExistence(timeout: 15),
                      "The external paginated conversation must open without taking its writer lock")
        let composer = app.textFields["task.message"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10)); composer.tap()
        composer.typeText("Keep this unsent draft")
        updateFixture("background-reply")
        XCTAssertTrue(app.descendants(matching: .any)["item.fixture-external-final"].waitForExistence(timeout: 20),
                      "The open conversation did not update after another process persisted its reply")
        XCTAssertEqual(composer.value as? String, "Keep this unsent draft")
        XCTAssertFalse(app.staticTexts["notice"].exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "External conversation updates with draft retained"; screenshot.lifetime = .keepAlways; add(screenshot)
    }

    func testSimulatorFetchesLatestReplyWhenOpeningTaskAfterForeground() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Open before background update")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        let detail = app.descendants(matching: .any)["task.detail"]
        XCUIDevice.shared.press(.home)
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        let updateURL = pairingURL.deletingLastPathComponent().appendingPathComponent("background-reply")
        var request = URLRequest(url: updateURL); request.httpMethod = "POST"
        let updated = expectation(description: "Other client updated the isolated conversation")
        URLSession.shared.dataTask(with: request) { _, response, error in
            XCTAssertNil(error)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
            updated.fulfill()
        }.resume()
        wait(for: [updated], timeout: 10)
        app.activate()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertFalse(detail.exists)
        let row = prefixedElement(app, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.fixture-external-final"].waitForExistence(timeout: 20),
                      "Foreground return did not fetch the conversation changed by another client")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Latest conversation fetched on foreground"; screenshot.lifetime = .keepAlways; add(screenshot)
    }

    func testSimulatorFetchesNewTaskAfterForeground() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        XCUIDevice.shared.press(.home)
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent("background-task"))
        request.httpMethod = "POST"
        var threadID: String?
        let created = expectation(description: "Other client created an isolated conversation")
        URLSession.shared.dataTask(with: request) { data, response, error in
            XCTAssertNil(error)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
            if let data, let result = try? JSONSerialization.jsonObject(with: data) as? [String: String] {
                threadID = result["threadId"]
            }
            created.fulfill()
        }.resume()
        wait(for: [created], timeout: 15)
        let identifier = try XCTUnwrap(threadID)
        app.activate()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.\(identifier)"].waitForExistence(timeout: 20),
                      "Foreground return did not fetch the conversation created by another client")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "New conversation fetched on foreground"; screenshot.lifetime = .keepAlways; add(screenshot)
    }

    func testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        func updateFixture(_ path: String) {
            var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent(path))
            request.httpMethod = "POST"
            let updated = expectation(description: "Update isolated title fixture")
            URLSession.shared.dataTask(with: request) { _, response, error in
                XCTAssertNil(error)
                XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
                updated.fulfill()
            }.resume()
            wait(for: [updated], timeout: 10)
        }
        let rows = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row.pagination-"))
        var seen = Set<String>()
        var ordered: [String] = []
        func recordRows() {
            for row in rows.allElementsBoundByIndex {
                if seen.insert(row.identifier).inserted { ordered.append(row.identifier) }
            }
        }
        func scrollTo(_ element: XCUIElement) {
            for _ in 0..<60 {
                recordRows()
                if element.exists && element.isHittable && element.frame.midY < app.frame.maxY - 100 { return }
                app.swipeUp()
            }
            XCTFail("Title did not become visible: \(element.identifier)")
        }
        updateFixture("title-fixture")
        defer { updateFixture("list-fixture/reset") }
        let started = Date()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-18"].waitForExistence(timeout: 15))
        print("Latest title visible after refresh: \(Date().timeIntervalSince(started)) seconds")
        let firstScreenshot = XCTAttachment(screenshot: app.screenshot())
        firstScreenshot.name = "Latest five project titles"; firstScreenshot.lifetime = .keepAlways; add(firstScreenshot)
        scrollTo(app.buttons["tasks.chats.more"])
        let expectedProjects = (22...26).reversed().flatMap { project in
            (14...18).reversed().map { "tasks.row.pagination-project-thread-\(project)-\($0)" }
        }
        XCTAssertEqual(ordered.filter { $0.contains("project-thread") }, expectedProjects)
        XCTAssertEqual(ordered.filter { $0.contains("pagination-chat-") }, (36...40).reversed().map { "tasks.row.pagination-chat-\($0)" })
        XCTAssertTrue(app.buttons["tasks.projects.more"].exists)
        let firstProject = app.buttons["tasks.project.pagination-project-26"]
        for _ in 0..<30 {
            if firstProject.exists && firstProject.isHittable { break }
            app.swipeDown()
        }
        let more = app.buttons["tasks.project.pagination-project-26.more"]
        scrollTo(more)
        more.tap()
        let fifteenth = app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-4"]
        scrollTo(fifteenth)
        scrollTo(more)
        XCTAssertEqual(ordered.filter { $0.hasPrefix("tasks.row.pagination-project-thread-26-") }, (4...18).reversed().map { "tasks.row.pagination-project-thread-26-\($0)" })
        more.tap()
        let oldest = app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-1"]
        scrollTo(oldest)
        XCTAssertFalse(more.exists)
        XCTAssertEqual(ordered.filter { $0.hasPrefix("tasks.row.pagination-project-thread-26-") }, (1...18).reversed().map { "tasks.row.pagination-project-thread-26-\($0)" })
        let expandedScreenshot = XCTAttachment(screenshot: app.screenshot())
        expandedScreenshot.name = "One project's oldest titles after expansion"; expandedScreenshot.lifetime = .keepAlways; add(expandedScreenshot)
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-project-thread-26-1"].waitForExistence(timeout: 15))
    }

    func testSimulatorPaginatesRecentProjectsAndUnassignedChats() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        func updateFixture(_ path: String) {
            var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent(path))
            request.httpMethod = "POST"
            let updated = expectation(description: "Update isolated list fixture")
            URLSession.shared.dataTask(with: request) { _, response, error in
                XCTAssertNil(error)
                XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
                updated.fulfill()
            }.resume()
            wait(for: [updated], timeout: 10)
        }
        let projects = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.project.pagination-project-"))
        let chats = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row.pagination-chat-"))
        var seenProjects = Set<String>()
        var seenChats = Set<String>()
        var projectOrder: [String] = []
        var chatOrder: [String] = []
        func recordRows() {
            for element in projects.allElementsBoundByIndex {
                if seenProjects.insert(element.identifier).inserted { projectOrder.append(element.identifier) }
            }
            for element in chats.allElementsBoundByIndex {
                if seenChats.insert(element.identifier).inserted { chatOrder.append(element.identifier) }
            }
        }
        func scrollTo(_ element: XCUIElement) {
            for _ in 0..<60 {
                recordRows()
                if element.exists && element.isHittable && element.frame.midY < app.frame.maxY - 100 { return }
                app.swipeUp()
            }
            XCTFail("List element did not become visible: \(element.identifier)")
        }
        updateFixture("list-fixture")
        defer { updateFixture("list-fixture/reset") }
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let newest = app.buttons["tasks.project.pagination-project-26"]
        XCTAssertTrue(newest.waitForExistence(timeout: 20))
        func restartTraversal() {
            // Native List anchoring can place inserted rows above the current viewport.
            // Read each expanded list from its first row, rather than assuming an insertion offset.
            for _ in 0..<60 {
                if newest.exists && newest.isHittable { break }
                app.swipeDown()
            }
            XCTAssertTrue(newest.exists && newest.isHittable)
            seenProjects.removeAll(); seenChats.removeAll()
            projectOrder.removeAll(); chatOrder.removeAll()
        }
        let projectMore = app.buttons["tasks.projects.more"]
        let chatMore = app.buttons["tasks.chats.more"]
        scrollTo(projectMore)
        XCTAssertEqual(projectOrder, (22...26).reversed().map { "tasks.project.pagination-project-\($0)" })
        let initialScreenshot = XCTAttachment(screenshot: app.screenshot())
        initialScreenshot.name = "Five recent projects and project expansion"; initialScreenshot.lifetime = .keepAlways; add(initialScreenshot)
        for count in [15, 25, 26] {
            projectMore.tap()
            restartTraversal()
            scrollTo(count == 26 ? chatMore : projectMore)
            XCTAssertEqual(projectOrder, ((27-count)...26).reversed().map { "tasks.project.pagination-project-\($0)" })
        }
        XCTAssertFalse(projectMore.exists)
        XCTAssertEqual(chatOrder, (36...40).reversed().map { "tasks.row.pagination-chat-\($0)" },
                       "Expanding projects changed the initial five chats")
        let oldest = app.descendants(matching: .any)["tasks.row.pagination-chat-1"]
        for count in [15, 25, 35, 40] {
            chatMore.tap()
            restartTraversal()
            scrollTo(count == 40 ? oldest : chatMore)
            XCTAssertEqual(chatOrder, ((41-count)...40).reversed().map { "tasks.row.pagination-chat-\($0)" })
        }
        XCTAssertFalse(chatMore.exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Last expanded chats"; screenshot.lifetime = .keepAlways; add(screenshot)
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-chat-1"].waitForExistence(timeout: 20),
                      "Old conversation beyond the previous list cap did not load")
    }

    func testSimulatorShowsWorkspaceConversationInsideItsProject() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent("background-task"))
        request.httpMethod = "POST"
        var threadID: String?
        let created = expectation(description: "Other client created an isolated conversation")
        URLSession.shared.dataTask(with: request) { data, response, error in
            XCTAssertNil(error)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
            if let data, let result = try? JSONSerialization.jsonObject(with: data) as? [String: String] {
                threadID = result["threadId"]
            }
            created.fulfill()
        }.resume()
        wait(for: [created], timeout: 15)
        let identifier = try XCTUnwrap(threadID)
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.\(identifier)"].waitForExistence(timeout: 20),
                      "The project conversation was absent from the refreshed list")
        let project = prefixedButton(app, prefix: "tasks.project.")
        XCTAssertTrue(project.waitForExistence(timeout: 10)); project.tap()
        let row = app.descendants(matching: .any)["tasks.row.\(identifier)"]
        let hidden = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: row)
        wait(for: [hidden], timeout: 10)
        project.tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Workspace conversation displayed inside its project"; screenshot.lifetime = .keepAlways; add(screenshot)
    }

    func testSimulatorFetchesNewTaskWhenReturningToList() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("This test uses the isolated Simulator fixture")
#endif
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Keep another conversation open")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent("background-task"))
        request.httpMethod = "POST"
        var threadID: String?
        let created = expectation(description: "Other client created an isolated conversation")
        URLSession.shared.dataTask(with: request) { data, response, error in
            XCTAssertNil(error)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
            if let data, let result = try? JSONSerialization.jsonObject(with: data) as? [String: String] {
                threadID = result["threadId"]
            }
            created.fulfill()
        }.resume()
        wait(for: [created], timeout: 15)
        let identifier = try XCTUnwrap(threadID)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.\(identifier)"].waitForExistence(timeout: 20),
                      "Returning to the list did not fetch the conversation created by another client")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "New conversation fetched when returning to the list"; screenshot.lifetime = .keepAlways; add(screenshot)
    }

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
        let emptyChat = XCTAttachment(screenshot: app.screenshot())
        emptyChat.name = "New conversation opens the existing empty chat"; emptyChat.lifetime = .keepAlways; add(emptyChat)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 10))
        try startSimulatorConversation(app, promptText: "[success] Start the simulator conversation")

        let activity = prefixedElement(app, prefix: "turn.activity.fixture-turn-")
        let streamedCommand = prefixedElement(app, prefix: "item.fixture-command-")
        XCTAssertTrue(activity.waitForExistence(timeout: 10), "Streaming activity header did not appear")
        XCTAssertFalse(streamedCommand.exists, "Live commands must start inside a collapsed group")
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-commentary-").exists,
                      "Commentary must remain visible outside the work group")

        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 15), "Final answer did not stream into the conversation")
        let commandCollapsed = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: streamedCommand,
        )
        wait(for: [commandCollapsed], timeout: 10)
        XCTAssertTrue(
            prefixedButton(app, prefix: "turn.activity.fixture-turn-").exists,
            "Completed work did not become an expandable collapsed summary",
        )
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
        let second = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND identifier CONTAINS %@", "turn.activity.", ":fixture-next-command-")).firstMatch
        XCTAssertTrue(second.waitForExistence(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-next-command-").exists)
        XCTAssertLessThan(first.frame.minY, progress.frame.minY)
        XCTAssertLessThan(progress.frame.minY, second.frame.minY)
        let collapsed = XCTAttachment(screenshot: app.screenshot())
        collapsed.name = "Commands grouped between visible commentary"; collapsed.lifetime = .keepAlways; add(collapsed)
        second.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-next-command-").waitForExistence(timeout: 5))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists,
                       "Expanding one group must leave the earlier group collapsed")
        app.buttons.matching(NSPredicate(format: "label == %@", "pwd")).firstMatch.tap()
        XCTAssertTrue(app.staticTexts["GROUP_DETAIL_OUTPUT"].waitForExistence(timeout: 5))
        let expanded = XCTAttachment(screenshot: app.screenshot())
        expanded.name = "Selected command group and command details expanded"; expanded.lifetime = .keepAlways; add(expanded)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 20))
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-command-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-next-command-").exists)
        XCTAssertFalse(progress.exists, "Completed work must hide interim commentary")
        let completed = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        XCTAssertTrue(completed.label.contains("3秒 作業しました"))
        XCTAssertEqual(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "turn.activity.")).count, 1)
        let finished = XCTAttachment(screenshot: app.screenshot())
        finished.name = "Completed work automatically collapsed"; finished.lifetime = .keepAlways; add(finished)
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

        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch.waitForExistence(timeout: 15))
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

        let request = prefixedElement(app, prefix: "request.host-proxy-")
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
        for _ in 0..<10 { detail.swipeDown() }

        for prefix in [
            "item.fixture-plan-", "item.fixture-mcp-", "item.fixture-dynamic-",
            "item.fixture-collab-", "item.fixture-subagent-", "item.fixture-web-",
            "item.fixture-image-", "item.fixture-compaction-",
        ] {
            XCTAssertTrue(waitForPrefixedElement(app, prefix: prefix, scrolling: detail), prefix)
        }
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-sleep-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-review-in-").exists)
        XCTAssertFalse(prefixedElement(app, prefix: "item.fixture-review-out-").exists)
    }

    func testSimulatorOpensLongInterruptedHistoryAtLatestMessage() throws {
#if !targetEnvironment(simulator)
        throw XCTSkip("Simulator-only isolated long-history fixture")
#endif
        let app = try connectedSimulatorApp()
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        func fixture(_ path: String) throws {
            let done = expectation(description: path)
            var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent(path))
            request.httpMethod = "POST"
            URLSession.shared.dataTask(with: request) { _, response, error in
                XCTAssertNil(error)
                XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
                done.fulfill()
            }.resume()
            wait(for: [done], timeout: 10)
        }
        try fixture("long-conversation")
        defer { try? fixture("list-fixture/reset") }
        app.terminate(); app.launch()
        let row = app.descendants(matching: .any)["tasks.row.fixture-long-history"]
        XCTAssertTrue(row.waitForExistence(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitForExistence(timeout: 30))
        let latest = app.descendants(matching: .any)["item.long-latest-message"]
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: latest)
        wait(for: [visible], timeout: 5)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Long interrupted history at latest message"; screenshot.lifetime = .keepAlways; add(screenshot)
        func loadedItems() -> Int {
            let value = detail.value as? String ?? ""
            return Int(value.components(separatedBy: "items=").last ?? "") ?? -1
        }
        XCTAssertEqual(loadedItems(), 500, "Initial history is five turns with a total budget of 500 items")
        for _ in 0..<40 {
            if loadedItems() > 500 { break }
            detail.swipeDown(velocity: .fast)
        }
        XCTAssertGreaterThan(loadedItems(), 500, "Scrolling upward must load older items without tapping a button")
        XCTAssertFalse(latest.isHittable, "Prepending history must not jump back to the latest message")
        let olderScreenshot = XCTAttachment(screenshot: app.screenshot())
        olderScreenshot.name = "Older history loaded by scrolling"; olderScreenshot.lifetime = .keepAlways; add(olderScreenshot)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        XCTAssertTrue(latest.waitForExistence(timeout: 20))
        XCTAssertTrue(latest.isHittable)
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
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Reference conversation layout"; screenshot.lifetime = .keepAlways; add(screenshot)
        let activity = prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        activity.tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-command-").waitForExistence(timeout: 5))
        let expanded = XCTAttachment(screenshot: app.screenshot())
        expanded.name = "Expanded work rows"; expanded.lifetime = .keepAlways; add(expanded)
        let command = app.buttons.matching(NSPredicate(format: "label == %@", "./gradlew test")).firstMatch
        command.tap()
        let fullOutput = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "DEFERRED_DETAIL_FULL_TEXT")).firstMatch
        XCTAssertTrue(fullOutput.waitForExistence(timeout: 10), "Expanding a deferred activity must fetch the full body")
        let detail = XCTAttachment(screenshot: app.screenshot())
        detail.name = "Deferred activity loaded"; detail.lifetime = .keepAlways; add(detail)
        command.tap()
        // Activity headers now leave the accessibility tree when virtualized.
        // Scroll back to the real control before collapsing its work rows.
        for _ in 0..<10 {
            if activity.exists && activity.isHittable { break }
            app.descendants(matching: .any)["task.detail"].swipeDown()
        }
        XCTAssertTrue(activity.exists && activity.isHittable)
        activity.tap()
        app.buttons["回答をコピー"].firstMatch.tap()
        XCTAssertTrue(app.buttons["コピーしました"].firstMatch.exists)
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
        app.buttons["files.diff"].tap()
        XCTAssertTrue(app.staticTexts["作業中の差分"].waitForExistence(timeout: 10))
        app.buttons["files.diff.close"].tap()
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
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Accepted additional input before native echo"; screenshot.lifetime = .keepAlways; add(screenshot)
        let pairingURL = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        var request = URLRequest(url: pairingURL.deletingLastPathComponent().appendingPathComponent("release-inputs"))
        request.httpMethod = "POST"
        let released = expectation(description: "Release the held native user-message echo")
        URLSession.shared.dataTask(with: request) { _, response, error in
            XCTAssertNil(error)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
            released.fulfill()
        }.resume()
        wait(for: [released], timeout: 10)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-steer-recorded-").waitForExistence(timeout: 10))
        XCTAssertEqual(app.staticTexts.matching(NSPredicate(format: "label == %@", "Show this additional input immediately")).count, 1)
        prefixedButton(app, prefix: "turn.interrupt.").tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch.waitForExistence(timeout: 15))
        XCTAssertEqual(app.staticTexts.matching(NSPredicate(format: "label == %@", "Show this additional input immediately")).count, 1)
    }

    func testSimulatorCanSteerAndStopAnActiveTurn() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[approval] Hold this turn")
        XCTAssertTrue(app.buttons["request.accept"].waitForExistence(timeout: 15))
        let active = XCTAttachment(screenshot: app.screenshot())
        active.name = "Active conversation text and tool labels"; active.lifetime = .keepAlways; add(active)
        let message = app.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Change the requested approach")
        app.buttons["task.send"].tap()
        XCTAssertTrue(app.staticTexts["Change the requested approach"].waitForExistence(timeout: 2))
        let stop = prefixedButton(app, prefix: "turn.interrupt.")
        XCTAssertTrue(stop.waitForExistence(timeout: 5)); stop.tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "3秒 作業した後に中断しました")).firstMatch.waitForExistence(timeout: 15))
        XCTAssertFalse(app.buttons["request.accept"].exists)
        XCTAssertFalse(stop.exists)
    }

    func testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[images] Display the attached images")
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 25))
        let finalID = finalAnswer.identifier
        let messageImage = app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image.")).firstMatch
        let hostImage = app.images["Hostから読み込んだ画像"]
        let inlineImage = app.images["インライン画像"]
        func show(_ image: XCUIElement, upward: Bool) {
            for _ in 0..<12 {
                if image.exists && image.isHittable { return }
                if upward { app.swipeUp() } else { app.swipeDown() }
            }
            XCTAssertTrue(image.waitForExistence(timeout: 10))
            XCTAssertTrue(image.isHittable)
        }
        show(inlineImage, upward: true)
        let response = XCTAttachment(screenshot: app.screenshot())
        response.name = "Markdown image decoded in the conversation"; response.lifetime = .keepAlways; add(response)
        show(hostImage, upward: false)
        show(messageImage, upward: false)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "Host attachment decoded in the user message"; attachment.lifetime = .keepAlways; add(attachment)
        app.terminate(); app.launch()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 30))
        let threadNumber = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(threadNumber)"]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        show(inlineImage, upward: true)
        show(hostImage, upward: false)
        show(messageImage, upward: false)
    }

    func testSimulatorCanAddASecondPhoto() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        let folder = app.buttons["task.folder"]
        XCTAssertTrue(folder.waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["task.environment"].isHittable)
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertLessThan(folder.frame.maxY, prompt.frame.minY)
        folder.tap()
        app.buttons["チャット"].tap()
        XCTAssertEqual(folder.label, "フォルダ: チャット")
        folder.tap()
        app.buttons["検証プロジェクト"].tap()
        XCTAssertEqual(folder.label, "フォルダ: 検証プロジェクト")
        let context = XCTAttachment(screenshot: app.screenshot())
        context.name = "New chat environment and folder above composer"; context.lifetime = .keepAlways; add(context)
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        for count in 1...2 {
            app.buttons["task.attach"].tap()
            app.buttons["task.attach.photos"].tap()
            let photo = app.images.matching(NSPredicate(format: "label CONTAINS %@", "写真")).firstMatch
            XCTAssertTrue(photo.waitForExistence(timeout: 15))
            photo.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            app.buttons["Add"].tap()
            let attached = expectation(for: NSPredicate(format: "count == %d", count), evaluatedWith: removals)
            wait(for: [attached], timeout: 60)
            XCTAssertTrue(app.buttons["task.attach"].isEnabled)
        }
        let attached = XCTAttachment(screenshot: app.screenshot())
        attached.name = "Two separately selected photos staged"; attached.lifetime = .keepAlways; add(attached)
        prompt.tap(); prompt.typeText("[success] Read both photos")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 30))
        XCTAssertEqual(removals.count, 0)
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.photos"].tap()
        let nextPhoto = app.images.matching(NSPredicate(format: "label CONTAINS %@", "写真")).firstMatch
        XCTAssertTrue(nextPhoto.waitForExistence(timeout: 15))
        nextPhoto.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        app.buttons["Add"].tap()
        let nextAttachment = expectation(for: NSPredicate(format: "count == 1"), evaluatedWith: removals)
        wait(for: [nextAttachment], timeout: 60)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
    }

    func testSimulatorCanAttachPhotosAndVideos() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        app.buttons["task.attach"].tap()
        XCTAssertTrue(app.buttons["task.attach.photos"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["task.attach.camera"].exists)
        XCTAssertTrue(app.buttons["task.attach.file"].exists)
        let menu = XCTAttachment(screenshot: app.screenshot())
        menu.name = "Photo video camera and file attachment menu"; menu.lifetime = .keepAlways; add(menu)
        app.buttons["task.attach.camera"].tap()
        XCTAssertTrue(app.staticTexts["この端末ではカメラを利用できません。"].waitForExistence(timeout: 5))
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.photos"].tap()
        for label in ["写真", "ビデオ"] {
            let media = app.images.matching(NSPredicate(format: "label CONTAINS %@", label)).firstMatch
            XCTAssertTrue(media.waitForExistence(timeout: 15), app.debugDescription)
            media.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
        let library = XCTAttachment(screenshot: app.screenshot())
        library.name = "Photo and video selected together"; library.lifetime = .keepAlways; add(library)
        app.buttons["Add"].tap()
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        let attached = expectation(for: NSPredicate(format: "count == 2"), evaluatedWith: removals)
        wait(for: [attached], timeout: 60)
        let prompt = app.descendants(matching: .any)["task.message"]
        prompt.tap(); prompt.typeText("[success] Read selected media")
        app.buttons["task.send"].tap()
        let photo = app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image.")).firstMatch
        XCTAssertTrue(photo.waitForExistence(timeout: 30), "Uploaded image did not decode in the conversation")
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "添付:")).firstMatch.waitForExistence(timeout: 20))
        XCTAssertEqual(removals.count, 0)
        let sent = XCTAttachment(screenshot: app.screenshot())
        sent.name = "Uploaded photo and video in chat history"; sent.lifetime = .keepAlways; add(sent)
    }

    func testSimulatorCanAttachDownloadAndPrepareAIEdit() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Prepare file operations")
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").waitForExistence(timeout: 20))
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.file"].tap()
        let browse = app.buttons["ブラウズ"]
        if browse.waitForExistence(timeout: 3) { browse.tap() }
        let onPhone = app.staticTexts["このiPhone内"]
        if onPhone.waitForExistence(timeout: 3) { onPhone.tap() }
        let folder = app.staticTexts["Bex"]
        if folder.waitForExistence(timeout: 3) { folder.tap() }
        let attachment = app.cells["attachment-fixture, txt"]
        XCTAssertTrue(attachment.waitForExistence(timeout: 10), app.debugDescription)
        attachment.tap()
        XCTAssertTrue(app.staticTexts["attachment-fixture.txt"].waitForExistence(timeout: 15))
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        XCTAssertTrue(reopened.staticTexts["attachment-fixture.txt"].waitForExistence(timeout: 10))
        let message = reopened.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Read the attached file")
        reopened.buttons["task.send"].tap()
        let historyAttachment = reopened.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "添付: attachment-fixture.txt")).firstMatch
        XCTAssertTrue(historyAttachment.waitForExistence(timeout: 15))
        openFiles(reopened)
        let file = reopened.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitForExistence(timeout: 10)); file.tap()
        XCTAssertTrue(reopened.textViews["file.editor"].waitForExistence(timeout: 10))
        reopened.buttons["ダウンロード"].tap()
        let shareName = reopened.otherElements["LP.CaptionBar.TopCaption"]
        XCTAssertTrue(shareName.waitForExistence(timeout: 15), reopened.debugDescription)
        XCTAssertEqual(shareName.label, "hello")
        let screenshot = XCTAttachment(screenshot: reopened.screenshot())
        screenshot.name = "Downloaded file in native share sheet"; screenshot.lifetime = .keepAlways; add(screenshot)
        reopened.buttons["header.closeButton"].tap()
        XCTAssertTrue(file.waitForExistence(timeout: 10)); file.tap()
        XCTAssertTrue(reopened.buttons["file.ai-edit"].waitForExistence(timeout: 10))
        reopened.buttons["file.ai-edit"].tap()
        XCTAssertTrue(message.waitForExistence(timeout: 10))
        XCTAssertTrue((message.value as? String ?? "").contains("hello.txt"))
    }

    func testPhysicalDeviceCanPairWithManualPayload() throws {
        guard let payload = ProcessInfo.processInfo.environment["BEX_PAIRING_PAYLOAD"],
              !payload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw XCTSkip("BEX_PAIRING_PAYLOAD is required for the physical-device pairing test")
        }

        let app = XCUIApplication()
        app.launch()
        allowFirstSystemPermissionIfPresent()

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

        let taskList = app.descendants(matching: .any)["tasks.list"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not appear after pairing")
        XCTAssertFalse(app.staticTexts["PCとペアリング"].exists)

        let notice = app.staticTexts["notice"]
        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(
            firstTask.waitForExistence(timeout: 30),
            "No task row appeared in the task list; notice: \(notice.exists ? notice.label : "(none)")",
        )
        firstTask.tap()

        let taskDetail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Task detail content did not appear; notice: \(notice.exists ? notice.label : "(none)")",
        )

        let loadingText = app.staticTexts["タスクを読み込み中…"]
        let loadingFinished = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: loadingText,
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
            "No rendered task item appeared; detail=\(detailMetrics)",
        )

        let backButton = app.navigationBars.buttons.element(boundBy: 0)
        let composer = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(
            composer.exists && composer.isHittable,
            "Task composer was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )
        XCTAssertTrue(
            backButton.exists && backButton.isHittable,
            "Task back button was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )

        app.terminate()
        app.launch()

        let reconnectTaskDetail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Relaunch did not open the task list")
        XCTAssertFalse(reconnectTaskDetail.exists)
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitForExistence(timeout: 30))
    }

    func testOpeningTaskAndReturningShowsTaskList() {
        let app = XCUIApplication()
        app.terminate()
        app.launch()

        let taskList = app.descendants(matching: .any)["tasks.list"]
        let taskDetail = app.descendants(matching: .any)["task.detail"]
        let connectionNotice = app.staticTexts["notice"]
        let ready = expectation(for: NSPredicate { _, _ in taskList.exists || taskDetail.exists }, evaluatedWith: app)
        wait(for: [ready], timeout: 30)
        if taskDetail.exists { app.navigationBars.buttons.element(boundBy: 0).tap() }
        XCTAssertTrue(taskList.waitForExistence(timeout: 30))

        let firstTask = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "tasks.row."))
            .firstMatch
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30), "No task row appeared in the task list")
        firstTask.tap()

        XCTAssertTrue(
            taskDetail.waitForExistence(timeout: 30),
            "Task detail content did not appear; notice: \(connectionNotice.exists ? connectionNotice.label : "(none)")",
        )

        let loadingText = app.staticTexts["タスクを読み込み中…"]
        let loadingFinished = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: loadingText,
        )
        let loadingResult = XCTWaiter.wait(for: [loadingFinished], timeout: 30)
        XCTAssertEqual(
            loadingResult,
            .completed,
            "Task detail remained on the loading screen",
        )

        let detailMetrics = taskDetail.value as? String ?? "(unavailable)"
        let firstItem = app
            .descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", "item."))
            .firstMatch
        XCTAssertTrue(
            firstItem.waitForExistence(timeout: 30),
            "No rendered task item appeared; detail=\(detailMetrics)",
        )

        let backButton = app.navigationBars.buttons.element(boundBy: 0)
        let composer = app.descendants(matching: .any)["task.message"]
        XCTAssertTrue(
            composer.exists && composer.isHittable,
            "Task composer was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )
        XCTAssertTrue(
            backButton.exists && backButton.isHittable,
            "Task back button was not visible and hittable after task detail loaded; detail=\(detailMetrics)",
        )
        backButton.tap()

        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Task list did not reappear after returning")
        XCTAssertTrue(firstTask.waitForExistence(timeout: 30)); firstTask.tap()
        XCTAssertTrue(firstItem.waitForExistence(timeout: 30))
        let navigationScreenshot = XCTAttachment(screenshot: app.screenshot())
        navigationScreenshot.name = "Native conversation navigation"
        navigationScreenshot.lifetime = .keepAlways; add(navigationScreenshot)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.005, dy: 0.4))
            .press(forDuration: 0.1, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.4)))
        XCTAssertTrue(taskList.waitForExistence(timeout: 30), "Native edge swipe did not return to the task list")
        XCTAssertFalse(taskDetail.exists)
    }

    private func openFiles(_ app: XCUIApplication) {
        app.buttons["task.more"].tap()
        let files = app.buttons["task.files"]
        XCTAssertTrue(files.waitForExistence(timeout: 5)); files.tap()
    }

    private func connectedSimulatorApp() throws -> XCUIApplication {
        let app = XCUIApplication()
        app.launch()

        let taskList = app.descendants(matching: .any)["tasks.list"]
        let taskDetail = app.descendants(matching: .any)["task.detail"]
        if taskDetail.waitForExistence(timeout: 2) { app.navigationBars.buttons.element(boundBy: 0).tap() }
        if taskList.waitForExistence(timeout: 3) { return app }

        let manualPairing = app.buttons["QRの内容を手入力"]
        if manualPairing.waitForExistence(timeout: 3) {
            let payload = try simulatorPairingPayload()
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
        }

        let notice = app.staticTexts["notice"]
        XCTAssertTrue(
            taskList.waitForExistence(timeout: 30),
            "Task list did not appear after connecting; notice: \(notice.exists ? notice.label : "(none)")"
        )
        return app
    }

    private func startSimulatorConversation(_ app: XCUIApplication, promptText: String) throws {
        let projectCompose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(projectCompose.waitForExistence(timeout: 10), "No project creation button appeared: \(app.debugDescription)")
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
            "Conversation did not open; notice: \(notice.exists ? notice.label : "(none)")",
        )
        XCTAssertTrue(app.descendants(matching: .any)["task.message"].isHittable)
    }

    private func prefixedElement(_ app: XCUIApplication, prefix: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
            .firstMatch
    }

    private func prefixedButton(_ app: XCUIApplication, prefix: String) -> XCUIElement {
        app.buttons
            .matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
            .firstMatch
    }

    private func waitForPrefixedElement(
        _ app: XCUIApplication,
        prefix: String,
        scrolling scrollView: XCUIElement
    ) -> Bool {
        let element = prefixedElement(app, prefix: prefix)
        if element.waitForExistence(timeout: 1) { return true }
        for _ in 0..<14 {
            scrollView.swipeUp()
            if element.waitForExistence(timeout: 1) { return true }
        }
        return false
    }

    private func allowFirstSystemPermissionIfPresent() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let alert = springboard.alerts.firstMatch
        guard alert.waitForExistence(timeout: 3) else { return }

        let positiveLabels = [
            "許可",
            "Allow",
            "Allow While Using App",
            "Allow Once",
            "許可する",
            "一度だけ許可",
        ]
        for label in positiveLabels {
            let button = alert.buttons[label]
            if button.exists {
                button.tap()
                return
            }
        }
    }

    private func simulatorPairingPayload() throws -> String {
        let url = try XCTUnwrap(URL(string: try XCTUnwrap(ProcessInfo.processInfo.environment["BEX_PAIRING_URL"])))
        let data = try Data(contentsOf: url)
        let payload = try XCTUnwrap(String(data: data, encoding: .utf8))
        guard !payload.isEmpty else { throw PairingPayloadError.invalidResponse }
        return payload
    }
}

private enum PairingPayloadError: Error {
    case invalidResponse
}
