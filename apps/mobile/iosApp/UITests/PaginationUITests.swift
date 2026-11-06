import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorOpensLongInterruptedHistoryAtLatestMessage() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("Simulator-only isolated long-history fixture")
        #endif
        let app = try connectedSimulatorApp()
        let conversationId = try useSimulatorConversationFixture("long-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.codex:\(conversationId)"]
        XCTAssertTrue(row.waitUntilExists(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitUntilExists(timeout: 30))
        let latest = app.descendants(matching: .any)["item.long-14-0"]
        XCTAssertTrue(latest.waitUntilExists(timeout: 20))
        let visible = expectation(for: NSPredicate(format: "isHittable == true"), evaluatedWith: latest)
        wait(for: [visible], timeout: 5)
        captureScreen(app, named: "Long interrupted history at latest message")
        let initialItems = loadedItems(in: detail)
        XCTAssertGreaterThan(initialItems, 0)
        let loadEarlier = app.buttons["history.load"]
        for _ in 0 ..< 40 {
            if loadEarlier.isHittable {
                break
            }
            detail.swipeDown(velocity: .fast)
        }
        XCTAssertTrue(loadEarlier.isHittable)
        XCTAssertEqual(loadedItems(in: detail), initialItems,
                       "Scrolling reveals loaded history without fetching another page")
        // SwiftUI can omit the outer group from AX after an insertion; measure its native text view.
        let anchor = app.textViews.matching(NSPredicate(
            format: "identifier == %@ AND value == %@", "message.user-text", "Review section 5, input 0."
        )).firstMatch
        XCTAssertTrue(anchor.isHittable)
        waitForStableFrame(anchor)
        let readingPosition = anchor.frame.minY
        loadEarlier.tap()
        let loaded = expectation(for: NSPredicate { _, _ in self.loadedItems(in: detail) > initialItems },
                                 evaluatedWith: detail)
        wait(for: [loaded], timeout: 20)
        waitForStableFrame(anchor)
        XCTAssertEqual(anchor.frame.minY, readingPosition, accuracy: 3,
                       "Prepending history must preserve the visible message position")
        XCTAssertFalse(latest.isHittable, "Prepending history must not jump back to the latest message")
        XCTAssertFalse(loadEarlier.exists, "The control disappears after the oldest page is loaded")
        captureScreen(app, named: "Earlier turns loaded without moving the reader")
        assertHistoryTopNavigation(app, latest: latest)
        XCTAssertTrue(latest.isHittable)
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        XCTAssertTrue(latest.waitUntilExists(timeout: 20))
        XCTAssertTrue(latest.isHittable, "Every reopen must render the latest message in the viewport")
    }

    private func assertHistoryTopNavigation(_ app: XCUIApplication, latest: XCUIElement) {
        let latestButton = app.buttons["task.latest"]
        XCTAssertTrue(latestButton.waitUntilExists(timeout: 5))
        latestButton.tap()
        XCTAssertTrue(latest.isHittable)
        XCTAssertFalse(latestButton.exists)
        tapStatusBar(app)
        XCTAssertTrue(latestButton.waitUntilExists(timeout: 5))
        XCTAssertFalse(latest.isHittable, "Top navigation must not snap back to the latest message")
        latestButton.tap()
    }

    func testSimulatorLoadsEarlierHistoryOnTapAtTheReadingPosition() throws {
        let app = try connectedSimulatorApp()
        let conversationId = try useSimulatorConversationFixture("viewport-conversation")
        app.terminate(); app.launch()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.codex:\(conversationId)"]
        XCTAssertTrue(row.waitUntilExists(timeout: 30)); row.tap()
        let detail = app.descendants(matching: .any)["task.detail"]
        XCTAssertTrue(detail.waitUntilExists(timeout: 30))
        let latest = app.descendants(matching: .any)["item.long-latest-message"]
        XCTAssertTrue(latest.waitUntilExists(timeout: 20))
        let loadEarlier = app.buttons["history.load"]
        XCTAssertTrue(loadEarlier.waitUntilExists(timeout: 10))
        XCTAssertTrue(loadEarlier.isHittable, "A short initial page must keep the manual control visible")
        let initialItems = loadedItems(in: detail)
        XCTAssertGreaterThan(initialItems, 0)
        XCTAssertTrue(latest.isHittable)
        // Older pages come from the Host transcript, not a native read that can
        // fail; page failure presentation is covered by the shared core tests.
        waitForStableFrame(latest)
        let readingPosition = latest.frame.minY
        let text = app.textViews.matching(NSPredicate(
            format: "value == %@", "Latest interrupted conversation message is visible."
        )).firstMatch
        assertConversationTextMargins(text, in: app)
        loadEarlier.tap()
        let loaded = expectation(for: NSPredicate { _, _ in self.loadedItems(in: detail) > initialItems },
                                 evaluatedWith: detail)
        wait(for: [loaded], timeout: 20)
        XCTAssertFalse(app.staticTexts["history.error"].exists)
        XCTAssertFalse(loadEarlier.exists)
        waitForStableFrame(latest)
        XCTAssertEqual(latest.frame.minY, readingPosition, accuracy: 3)
        XCTAssertTrue(latest.isHittable, "Loading a page must preserve the current reading position")
        assertConversationTextMargins(text, in: app)
        captureScreen(app, named: "Earlier history preserved the latest message position")
    }

    private func loadedItems(in detail: XCUIElement) -> Int {
        let value = detail.value as? String ?? ""
        return Int(value.components(separatedBy: "items=").last ?? "") ?? -1
    }

    func testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("title-fixture")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-16"]
            .waitUntilExists(timeout: 20))
        scrollToListElement(
            app.descendants(matching: .any)["tasks.row.codex:pagination-chat-12"],
            in: app
        )
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-11"].exists)
        let project = app.buttons["tasks.project.pagination-project-16"]
        scrollToListElement(project, in: app)
        XCTAssertEqual(project.value as? String, "閉じています")
        project.tap()
        let newest = app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-16"]
        XCTAssertTrue(newest.waitUntilExists(timeout: 15))
        let lastInitial = app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-12"]
        scrollToListElement(lastInitial, in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-11"]
            .exists)
        let more = app.buttons["tasks.project.pagination-project-16.more"]
        scrollToListElement(more, in: app)
        more.tap()
        scrollToEarlierListElement(project, in: app, attempts: 30)
        scrollToListElement(
            app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-6"],
            in: app
        )
        scrollToListElement(more, in: app)
        more.tap()
        scrollToEarlierListElement(project, in: app, attempts: 30)
        scrollToListElement(
            app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-1"],
            in: app
        )
        XCTAssertFalse(more.exists)
        let oldest = app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-1"]
        assertExpandedProjectSurvivesNavigation(app, project: project, oldest: oldest, more: more)
        assertOtherProjectRemainsAtInitialPage(in: app)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Only the selected project's titles expanded and retained after returning")
    }

    private func assertExpandedProjectSurvivesNavigation(_ app: XCUIApplication, project: XCUIElement,
                                                         oldest: XCUIElement, more: XCUIElement) {
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-project-thread-16-1"]
            .waitUntilExists(timeout: 15))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 15))
        scrollToEarlierListElement(project, in: app, attempts: 30)
        XCTAssertEqual(project.value as? String, "開いています")
        scrollToListElement(oldest, in: app)
        XCTAssertFalse(more.exists)
    }

    private func assertOtherProjectRemainsAtInitialPage(in app: XCUIApplication) {
        let otherProject = app.buttons["tasks.project.pagination-project-15"]
        scrollToListElement(otherProject, in: app)
        XCTAssertEqual(otherProject.value as? String, "閉じています")
        otherProject.tap()
        let otherLastInitial = app
            .descendants(matching: .any)["tasks.row.codex:pagination-project-thread-15-12"]
        scrollToListElement(otherLastInitial, in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-15-11"]
            .exists)
        scrollToListElement(app.buttons["tasks.project.pagination-project-15.more"], in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-15-11"]
            .exists)
    }

    func testSimulatorPaginatesRecentProjectsAndUnassignedChats() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("list-fixture")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let newest = app.buttons["tasks.project.pagination-project-16"]
        XCTAssertTrue(newest.waitUntilExists(timeout: 20))
        scrollToListElement(app.buttons["tasks.project.pagination-project-12"], in: app)
        XCTAssertFalse(app.buttons["tasks.project.pagination-project-11"].exists)
        let projectMore = app.buttons["tasks.projects.more"]
        for last in [2, 1] {
            scrollToListElement(projectMore, in: app)
            projectMore.tap()
            scrollToEarlierListElement(newest, in: app, attempts: 30)
            scrollToListElement(app.buttons["tasks.project.pagination-project-\(last)"], in: app)
        }
        XCTAssertFalse(projectMore.exists)

        let newestChat = app.descendants(matching: .any)["tasks.row.codex:pagination-chat-16"]
        XCTAssertTrue(newestChat.waitUntilExists(timeout: 20))
        scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-12"], in: app)
        let chatMore = app.buttons["tasks.chats.more"]
        scrollToListElement(chatMore, in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-11"].exists,
                       "Expanding projects must leave chats at the initial five")
        for last in [2, 1] {
            scrollToListElement(chatMore, in: app)
            chatMore.tap()
            scrollToEarlierListElement(newestChat, in: app, attempts: 30)
            scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-\(last)"], in: app)
        }
        XCTAssertFalse(chatMore.exists)
        app.descendants(matching: .any)["tasks.row.codex:pagination-chat-1"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-chat-1"]
            .waitUntilExists(timeout: 20))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Oldest chat opened after separate project and chat pagination")
    }

    func scrollToListElement(_ element: XCUIElement, in app: XCUIApplication, bottomMargin: CGFloat = 100) {
        for _ in 0 ..< 60 {
            if element.exists, element.isHittable, element.frame.midY < app.frame.maxY - bottomMargin {
                return
            }
            let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65))
            let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35))
            start.press(forDuration: 0.1, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.1)
        }
        XCTFail("List element did not become visible: \(element.identifier)")
    }

    func scrollToEarlierListElement(_ element: XCUIElement, in app: XCUIApplication, attempts: Int) {
        for _ in 0 ..< attempts {
            // SwiftUI can report rows behind the navigation bar as hittable.
            let visibleTop = app.navigationBars.allElementsBoundByIndex.map(\.frame.maxY).max() ?? app.frame.minY
            if element.exists, element.isHittable, element.frame.minY >= visibleTop {
                return
            }
            app.swipeDown()
        }
        XCTFail("Earlier list element did not become visible: \(element.identifier)")
    }
}
