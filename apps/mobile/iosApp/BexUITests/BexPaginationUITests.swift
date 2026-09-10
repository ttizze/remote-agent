import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let rows = app.descendants(matching: .any).matching(NSPredicate(
            format: "identifier BEGINSWITH %@",
            "tasks.row.pagination-"
        ))
        let observed = ObservedListRows(rows)
        func scrollTo(_ element: XCUIElement) {
            scrollToListElement(element, in: app, recording: observed.record)
        }
        try useSimulatorListFixture("title-fixture")
        let started = Date()
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        verifyInitialTitles(app, observed: observed, started: started)
        let firstProject = app.buttons["tasks.project.pagination-project-26"]
        func restartTraversal() {
            // Inserted titles can move above the viewport while the More row stays visible.
            scrollToEarlierListElement(firstProject, in: app, attempts: 30)
            XCTAssertTrue(firstProject.exists && firstProject.isHittable)
            observed.reset()
        }
        scrollToEarlierListElement(firstProject, in: app, attempts: 30)
        let more = app.buttons["tasks.project.pagination-project-26.more"]
        scrollTo(more)
        more.tap()
        restartTraversal()
        let fifteenth = app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-4"]
        scrollTo(fifteenth)
        scrollTo(more)
        XCTAssertEqual(
            observed.order.filter { $0.hasPrefix("tasks.row.pagination-project-thread-26-") },
            (4 ... 18).reversed().map { "tasks.row.pagination-project-thread-26-\($0)" }
        )
        more.tap()
        restartTraversal()
        let oldest = app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-1"]
        scrollTo(oldest)
        XCTAssertFalse(more.exists)
        XCTAssertEqual(
            observed.order.filter { $0.hasPrefix("tasks.row.pagination-project-thread-26-") },
            (1 ... 18).reversed().map { "tasks.row.pagination-project-thread-26-\($0)" }
        )
        captureScreen(app, named: "One project's oldest titles after expansion")
        verifyExpandedTitlesAfterReturning(app, observed: observed)
    }

    func testSimulatorPaginatesRecentProjectsAndUnassignedChats() throws {
        #if !targetEnvironment(simulator)
            throw XCTSkip("This test uses the isolated Simulator fixture")
        #endif
        let app = try connectedSimulatorApp()
        let projects = app.buttons.matching(NSPredicate(
            format: "identifier BEGINSWITH %@",
            "tasks.project.pagination-project-"
        ))
        let chats = app.descendants(matching: .any).matching(NSPredicate(
            format: "identifier BEGINSWITH %@",
            "tasks.row.pagination-chat-"
        ))
        let observedProjects = ObservedListRows(projects)
        let observedChats = ObservedListRows(chats)
        func scrollTo(_ element: XCUIElement) {
            scrollToListElement(element, in: app) {
                observedProjects.record()
                observedChats.record()
            }
        }
        try useSimulatorListFixture("list-fixture")
        app.buttons["tasks.menu"].tap()
        app.buttons["tasks.refresh"].tap()
        let newest = app.buttons["tasks.project.pagination-project-26"]
        XCTAssertTrue(newest.waitForExistence(timeout: 20))
        func restartTraversal() {
            // List insertions can move rows above the viewport; reread from the first row.
            scrollToEarlierListElement(newest, in: app, attempts: 60)
            XCTAssertTrue(newest.exists && newest.isHittable)
            observedProjects.reset()
            observedChats.reset()
        }
        let projectMore = app.buttons["tasks.projects.more"]
        let chatMore = app.buttons["tasks.chats.more"]
        scrollTo(projectMore)
        XCTAssertEqual(observedProjects.order, (22 ... 26).reversed().map { "tasks.project.pagination-project-\($0)" })
        captureScreen(app, named: "Five recent projects and project expansion")
        for count in [15, 25, 26] {
            projectMore.tap()
            restartTraversal()
            scrollTo(count == 26 ? chatMore : projectMore)
            XCTAssertEqual(
                observedProjects.order,
                ((27 - count) ... 26).reversed().map { "tasks.project.pagination-project-\($0)" }
            )
        }
        XCTAssertFalse(projectMore.exists)
        XCTAssertEqual(observedChats.order, (36 ... 40).reversed().map { "tasks.row.pagination-chat-\($0)" },
                       "Expanding projects changed the initial five chats")
        verifyChatExpansion(app, observed: observedChats, restartTraversal: restartTraversal)
    }

    func verifyInitialTitles(_ app: XCUIApplication, observed: ObservedListRows, started: Date) {
        func scrollTo(_ element: XCUIElement) {
            scrollToListElement(element, in: app, recording: observed.record)
        }
        for number in (22 ... 26).reversed() {
            let project = app.buttons["tasks.project.pagination-project-\(number)"]
            scrollTo(project)
            XCTAssertEqual(project.value as? String, "閉じています")
            project.tap()
        }
        scrollToEarlierListElement(app.buttons["tasks.project.pagination-project-26"], in: app, attempts: 30)
        observed.reset()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-18"]
            .waitForExistence(timeout: 15))
        print("Latest title visible after refresh: \(Date().timeIntervalSince(started)) seconds")
        captureScreen(app, named: "Latest five project titles")
        scrollTo(app.buttons["tasks.chats.more"])
        let expectedProjects = (22 ... 26).reversed().flatMap { project in
            (14 ... 18).reversed().map { "tasks.row.pagination-project-thread-\(project)-\($0)" }
        }
        XCTAssertEqual(observed.order.filter { $0.contains("project-thread") }, expectedProjects)
        XCTAssertEqual(
            observed.order.filter { $0.contains("pagination-chat-") },
            (36 ... 40).reversed().map { "tasks.row.pagination-chat-\($0)" }
        )
        XCTAssertTrue(app.buttons["tasks.projects.more"].exists)
    }

    private func verifyExpandedTitlesAfterReturning(_ app: XCUIApplication, observed: ObservedListRows) {
        let oldest = app.descendants(matching: .any)["tasks.row.pagination-project-thread-26-1"]
        let firstProject = app.buttons["tasks.project.pagination-project-26"]
        let more = app.buttons["tasks.project.pagination-project-26.more"]
        func scrollTo(_ element: XCUIElement) {
            scrollToListElement(element, in: app, recording: observed.record)
        }
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-project-thread-26-1"]
            .waitForExistence(timeout: 15))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 15))
        observed.reset()
        scrollToEarlierListElement(firstProject, in: app, attempts: 30)
        XCTAssertEqual(firstProject.value as? String, "開いています")
        scrollTo(oldest)
        XCTAssertFalse(more.exists)
        XCTAssertEqual(
            observed.order.filter { $0.hasPrefix("tasks.row.pagination-project-thread-26-") },
            (1 ... 18).reversed().map { "tasks.row.pagination-project-thread-26-\($0)" }
        )
        captureScreen(app, named: "Expanded titles retained after returning from detail")
    }

    private func verifyChatExpansion(_ app: XCUIApplication, observed: ObservedListRows,
                                     restartTraversal: () -> Void) {
        let oldest = app.descendants(matching: .any)["tasks.row.pagination-chat-1"]
        let chatMore = app.buttons["tasks.chats.more"]
        func scrollTo(_ element: XCUIElement) {
            scrollToListElement(element, in: app, recording: observed.record)
        }
        for count in [15, 25, 35, 40] {
            chatMore.tap()
            restartTraversal()
            scrollTo(count == 40 ? oldest : chatMore)
            XCTAssertEqual(observed.order, ((41 - count) ... 40).reversed().map { "tasks.row.pagination-chat-\($0)" })
        }
        XCTAssertFalse(chatMore.exists)
        captureScreen(app, named: "Last expanded chats")
        oldest.tap()
        XCTAssertTrue(app.descendants(matching: .any)["item.answer-pagination-chat-1"].waitForExistence(timeout: 20),
                      "Old conversation beyond the previous list cap did not load")
    }

    func scrollToListElement(_ element: XCUIElement, in app: XCUIApplication, recording record: () -> Void) {
        for _ in 0 ..< 60 {
            record()
            if element.exists, element.isHittable, element.frame.midY < app.frame.maxY - 100 {
                return
            }
            // Keep adjacent viewports overlapping so every lazy row is observed.
            let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.65))
            let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35))
            start.press(forDuration: 0.1, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.1)
        }
        XCTFail("List element did not become visible: \(element.identifier)")
    }

    func scrollToEarlierListElement(_ element: XCUIElement, in app: XCUIApplication, attempts: Int) {
        for _ in 0 ..< attempts {
            if element.exists, element.isHittable {
                break
            }
            app.swipeDown()
        }
    }
}

final class ObservedListRows {
    private let query: XCUIElementQuery
    private var seen = Set<String>()
    private(set) var order = [String]()

    init(_ query: XCUIElementQuery) {
        self.query = query
    }

    func record() {
        for element in query.allElementsBoundByIndex {
            let identifier = element.identifier
            if seen.insert(identifier).inserted {
                order.append(identifier)
            }
        }
    }

    func reset() {
        seen.removeAll()
        order.removeAll()
    }
}
