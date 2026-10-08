import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorLoadsRecentTitlesAndOpensOldProjectsOnDemand() throws {
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
        let newest = app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-16-16"]
        XCTAssertTrue(newest.waitForExistence(timeout: 15))
        scrollToListElement(app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-16-12"], in: app)
        let more = app.buttons["tasks.project.pagination-project-16.more"]
        for last in [2, 1] {
            scrollToListElement(more, in: app)
            more.tap()
            scrollToEarlierListElement(project, in: app, attempts: 30)
            scrollToListElement(
                app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-16-\(last)"],
                in: app
            )
        }
        XCTAssertFalse(more.exists)
        let oldest = app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-16-1"]
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
        scrollToListElement(app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-15-12"], in: app)
        scrollToListElement(app.buttons["tasks.project.pagination-project-15.more"], in: app)
        XCTAssertFalse(app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-15-11"].exists)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Only the selected project's titles expanded and retained after returning")
    }

    func testSimulatorPaginatesRecentTasksWithoutLoadingClosedProjects() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        try useSimulatorListFixture("list-fixture")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let recent = app.descendants(matching: .any)["tasks.row.codex:pagination-chat-16"]
        XCTAssertTrue(recent.waitForExistence(timeout: 20))
        let oldest = app.descendants(matching: .any)["tasks.row.codex:pagination-project-thread-1"]
        XCTAssertFalse(oldest.exists)
        let oldProject = app.buttons["tasks.project.pagination-project-1"]
        scrollToListElement(oldProject, in: app)
        XCTAssertEqual(oldProject.value as? String, "閉じています")
        let oldTask = app.descendants(matching: .any)["tasks.project.row.codex:pagination-project-thread-1"]
        XCTAssertFalse(oldTask.exists)
        oldProject.tap()
        scrollToListElement(oldTask, in: app)
        oldTask.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-project-thread-1"].waitForExistence(timeout: 20))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        scrollToEarlierListElement(recent, in: app, attempts: 60)
        let more = app.buttons["tasks.recent.more"]
        scrollToListElement(more, in: app)
        more.tap()
        scrollToEarlierListElement(recent, in: app, attempts: 30)
        scrollToListElement(oldest, in: app)
        XCTAssertFalse(more.exists)
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-project-thread-1"].waitForExistence(timeout: 20))
        XCTAssertFalse(app.staticTexts["notice"].exists)
        captureScreen(app, named: "Recent task pagination and old project loading remain independent")
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
