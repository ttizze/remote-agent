import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
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
        captureScreen(app, named: "New chat environment and folder above composer")
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        selectPhotos(["写真"], in: app)
        app.buttons["Cancel"].tap()
        XCTAssertEqual(removals.count, 0)
        XCTAssertTrue(app.buttons["task.attach"].isEnabled)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        for count in 1 ... 2 {
            selectPhotos(["写真"], in: app)
            app.buttons["Add"].tap()
            let attached = expectation(for: NSPredicate(format: "count == %d", count), evaluatedWith: removals)
            wait(for: [attached], timeout: 60)
            XCTAssertTrue(app.buttons["task.attach"].isEnabled)
        }
        captureScreen(app, named: "Two separately selected photos staged")
        prompt.tap(); prompt.typeText("[success] Read both photos")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 30))
        XCTAssertEqual(removals.count, 0)
        selectPhotos(["写真"], in: app)
        app.buttons["Add"].tap()
        let nextAttachment = expectation(for: NSPredicate(format: "count == 1"), evaluatedWith: removals)
        wait(for: [nextAttachment], timeout: 60)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
    }

    func testSimulatorCanAttachPhotosAndVideos() throws {
        let app = try connectedSimulatorApp()
        try useAutomaticWorktrees(app)
        let compose = app.buttons["tasks.new.chat"]
        XCTAssertTrue(compose.waitForExistence(timeout: 10)); compose.tap()
        XCTAssertEqual(app.buttons["task.folder"].label, "フォルダ: チャット")
        app.buttons["task.attach"].tap()
        XCTAssertTrue(app.buttons["task.attach.photos"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["task.attach.camera"].exists)
        XCTAssertTrue(app.buttons["task.attach.file"].exists)
        captureScreen(app, named: "Photo video camera and file attachment menu")
        app.buttons["task.attach.camera"].tap()
        XCTAssertTrue(app.staticTexts["この端末ではカメラを利用できません。"].waitForExistence(timeout: 5))
        selectPhotos(["写真", "ビデオ"], in: app)
        captureScreen(app, named: "Photo and video selected together")
        app.buttons["Add"].tap()
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        let attached = expectation(for: NSPredicate(format: "count == 2"), evaluatedWith: removals)
        wait(for: [attached], timeout: 60)
        let prompt = app.descendants(matching: .any)["task.message"]
        prompt.tap(); prompt.typeText("[success] Read selected media")
        app.buttons["task.send"].tap()
        let photo = app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image.")).firstMatch
        XCTAssertTrue(photo.waitForExistence(timeout: 30), "Uploaded image did not decode in the conversation")
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "添付:")).firstMatch
            .waitForExistence(timeout: 20))
        XCTAssertEqual(removals.count, 0)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 30))
        let completed = expectation(
            for: NSPredicate(format: "label CONTAINS %@", "件の過去のメッセージ"),
            evaluatedWith: prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        )
        wait(for: [completed], timeout: 10)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(app.staticTexts["notice"].exists, "Completed attachment send must not leave a Host error")
        captureScreen(app, named: "Uploaded photo and video in chat history")
    }

    func testSimulatorRetriesPhotoUploadAfterWorkspaceRecovery() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        try useSimulatorListFixture("worktree-conversation")
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.refresh"].tap()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.fixture-worktree-thread"]
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        let prompt = app.textFields["task.message"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap(); prompt.typeText("[success] Retry selected photos")
        try simulatorFixture("worktree/unavailable")
        var unavailable = true
        addTeardownBlock {
            if unavailable {
                try self.simulatorFixture("worktree/restore")
            }
        }
        selectPhotos(["写真", "ビデオ"], in: app)
        app.buttons["Add"].tap()
        let notices = app.staticTexts.matching(identifier: "notice")
        XCTAssertTrue(notices.firstMatch.waitForExistence(timeout: 30))
        let attach = app.buttons["task.attach"]
        let recovered = expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: attach)
        wait(for: [recovered], timeout: 10)
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        XCTAssertEqual(removals.count, 0)
        XCTAssertEqual(prompt.value as? String, "[success] Retry selected photos")
        try simulatorFixture("worktree/restore"); unavailable = false
        selectPhotos(["写真", "ビデオ"], in: app)
        app.buttons["Add"].tap()
        let attached = expectation(for: NSPredicate(format: "count == 2"), evaluatedWith: removals)
        wait(for: [attached], timeout: 60)
        XCTAssertEqual(notices.count, 0)
        app.buttons["task.send"].tap()
        let final = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(final.waitForExistence(timeout: 30))
        let finalID = final.identifier
        XCTAssertEqual(removals.count, 0)
        XCTAssertNotEqual(prompt.value as? String, "[success] Retry selected photos")
        app.terminate(); _ = try connectedSimulatorApp()
        XCTAssertTrue(row.waitForExistence(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitForExistence(timeout: 20))
        XCTAssertTrue(app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image."))
            .firstMatch.waitForExistence(timeout: 15))
        XCTAssertEqual(notices.count, 0)
        captureScreen(app, named: "Recovered media upload survives reopening")
    }

    private func selectPhotos(_ labels: [String], in app: XCUIApplication) {
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.photos"].tap()
        for label in labels {
            let media = app.images.matching(NSPredicate(format: "label CONTAINS %@", label)).firstMatch
            XCTAssertTrue(media.waitForExistence(timeout: 15), app.debugDescription)
            media.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
    }

    private func selectAttachmentFixture(in app: XCUIApplication) {
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.file"].tap()
        let attachment = app.cells["attachment-fixture, txt"]
        // The picker restores the app's Documents directory asynchronously.
        // Wait for that destination before navigating a transient Browse page.
        if !attachment.waitForExistence(timeout: 10) {
            let browse = app.buttons["ブラウズ"]
            if browse.waitForExistence(timeout: 3) {
                browse.tap()
            }
            let onPhone = app.staticTexts["このiPhone内"]
            if onPhone.waitForExistence(timeout: 3) {
                onPhone.tap()
            }
            let folder = app.staticTexts["Bex"]
            if folder.waitForExistence(timeout: 3) {
                folder.tap()
            }
        }
        XCTAssertTrue(attachment.waitForExistence(timeout: 10), app.debugDescription)
        attachment.tap()
    }

    func testSimulatorCanAttachDownloadAndPrepareAIEdit() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Prepare file operations")
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").waitForExistence(timeout: 20))
        selectAttachmentFixture(in: app)
        XCTAssertTrue(app.staticTexts["attachment-fixture.txt"].waitForExistence(timeout: 15))
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.fixture-thread-")
        XCTAssertTrue(row.waitForExistence(timeout: 10)); row.tap()
        XCTAssertTrue(reopened.staticTexts["attachment-fixture.txt"].waitForExistence(timeout: 10))
        let message = reopened.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Read the attached file")
        reopened.buttons["task.send"].tap()
        let historyAttachment = reopened.staticTexts.matching(NSPredicate(
            format: "label CONTAINS %@",
            "添付: attachment-fixture.txt"
        )).firstMatch
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
}
