import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("title-fixture")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let project = app.buttons["tasks.project.pagination-project-16"]
        XCTAssertTrue(project.waitForExistence(timeout: 20))
        XCTAssertEqual(project.value as? String, "閉じています")
        project.tap()
        let newest = app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-16"]
        XCTAssertTrue(newest.waitForExistence(timeout: 15))
        scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-12"], in: app)
        let more = app.buttons["tasks.project.pagination-project-16.more"]
        for last in [2, 1] {
            scrollToListElement(more, in: app)
            more.tap()
            scrollToEarlierListElement(project, in: app, attempts: 30)
            scrollToListElement(
                app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-\(last)"],
                in: app
            )
        }
        XCTAssertFalse(more.exists)
        let oldest = app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-16-1"]
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-project-thread-16-1"]
            .waitForExistence(timeout: 15))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        scrollToEarlierListElement(project, in: app, attempts: 30)
        XCTAssertEqual(project.value as? String, "開いています")
        scrollToListElement(oldest, in: app)
        XCTAssertFalse(more.exists)
        let otherProject = app.buttons["tasks.project.pagination-project-15"]
        scrollToListElement(otherProject, in: app)
        XCTAssertEqual(otherProject.value as? String, "閉じています")
        otherProject.tap()
        scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-15-12"], in: app)
        scrollToListElement(app.buttons["tasks.project.pagination-project-15.more"], in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-15-11"].exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Only the selected project's titles expanded and retained after returning")
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
        XCTAssertTrue(newest.waitForExistence(timeout: 20))
        scrollToListElement(app.buttons["tasks.project.pagination-project-12"], in: app)
        let projectMore = app.buttons["tasks.projects.more"]
        for last in [2, 1] {
            scrollToListElement(projectMore, in: app)
            projectMore.tap()
            scrollToEarlierListElement(newest, in: app, attempts: 30)
            scrollToListElement(app.buttons["tasks.project.pagination-project-\(last)"], in: app)
        }
        XCTAssertFalse(projectMore.exists)
        scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-16"], in: app)
        scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-12"], in: app)
        let chatMore = app.buttons["tasks.chats.more"]
        scrollToListElement(chatMore, in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-11"].exists,
                       "Expanding projects must leave chats at the initial five")
        for last in [2, 1] {
            scrollToListElement(chatMore, in: app)
            chatMore.tap()
            scrollToEarlierListElement(
                app.descendants(matching: .any)["tasks.row.codex:pagination-chat-16"],
                in: app,
                attempts: 30
            )
            scrollToListElement(app.descendants(matching: .any)["tasks.row.codex:pagination-chat-\(last)"], in: app)
        }
        XCTAssertFalse(chatMore.exists)
        app.descendants(matching: .any)["tasks.row.codex:pagination-chat-1"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-chat-1"].waitForExistence(timeout: 20))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Oldest chat opened after separate project and chat pagination")
    }

    func scrollToListElement(_ element: XCUIElement, in app: XCUIApplication) {
        for _ in 0 ..< 60 {
            if element.exists, element.isHittable, element.frame.midY < app.frame.maxY - 100 {
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
            if element.exists, element.isHittable {
                return
            }
            app.swipeDown()
        }
        XCTFail("Earlier list element did not become visible: \(element.identifier)")
    }
}
