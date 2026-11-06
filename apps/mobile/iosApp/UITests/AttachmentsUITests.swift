import Foundation
import XCTest

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorCanAddASecondPhoto() throws {
        let app = try connectedSimulatorApp()
        let compose = app.buttons["tasks.new.project.simulator-project"]
        XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
        let folder = app.buttons["task.folder"]
        XCTAssertTrue(folder.waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["task.environment"].isHittable)
        let prompt = app.descendants(matching: .any)["task.message"]
        XCTAssertLessThan(folder.frame.maxY, prompt.frame.minY)
        folder.tap()
        app.buttons["チャット"].tap()
        XCTAssertEqual(folder.label, "フォルダ: チャット")
        folder.tap()
        app.buttons["検証プロジェクト"].tap()
        XCTAssertEqual(folder.label, "フォルダ: 検証プロジェクト")
        captureScreen(app, named: "New chat project and environment in the centered prompt")
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        selectPhotos([.photo], in: app)
        app.navigationBars.buttons.matching(NSPredicate(format: "label IN %@", ["Cancel", "キャンセル"]))
            .firstMatch.tap()
        XCTAssertEqual(removals.count, 0)
        XCTAssertTrue(app.buttons["task.attach"].isEnabled)
        XCTAssertFalse(app.staticTexts["notice"].exists)
        for count in 1 ... 2 {
            selectPhotos([.photo], in: app)
            confirmPhotoSelection(in: app)
            let attached = expectation(for: NSPredicate(format: "count == %d", count), evaluatedWith: removals)
            wait(for: [attached], timeout: 60)
            XCTAssertTrue(app.buttons["task.attach"].isEnabled)
        }
        captureScreen(app, named: "Two separately selected photos staged")
        prompt.tap(); prompt.typeText("[success] Read both photos")
        app.buttons["task.send"].tap()
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 30))
        XCTAssertEqual(removals.count, 0)
        selectPhotos([.photo], in: app)
        confirmPhotoSelection(in: app)
        let nextAttachment = expectation(for: NSPredicate(format: "count == 1"), evaluatedWith: removals)
        wait(for: [nextAttachment], timeout: 60)
        XCTAssertTrue(app.buttons["task.send"].isEnabled)
        let thumbnail = app.images["composer.attachment.0"]
        XCTAssertTrue(thumbnail.waitUntilExists(timeout: 20))
        let remove = removals.firstMatch
        XCTAssertGreaterThan(remove.frame.midX, thumbnail.frame.midX)
        XCTAssertLessThan(remove.frame.midY, thumbnail.frame.midY)
        XCTAssertLessThanOrEqual(thumbnail.frame.maxY, prompt.frame.minY)
        prompt.tap(); prompt.typeText("Keep this draft")
        remove.tap()
        XCTAssertFalse(thumbnail.exists)
        XCTAssertEqual(removals.count, 0)
        XCTAssertEqual(prompt.value as? String, "Keep this draft")
    }

    func testSimulatorCanAttachPhotosAndVideos() throws {
        let app = try connectedSimulatorApp()
        try useAutomaticWorktrees(app)
        let compose = app.buttons["tasks.new.chat"]
        XCTAssertTrue(compose.waitUntilExists(timeout: 10)); compose.tap()
        XCTAssertEqual(app.buttons["task.folder"].label, "フォルダ: チャット")
        app.buttons["task.attach"].tap()
        XCTAssertTrue(app.buttons["task.attach.photos"].waitUntilExists(timeout: 5))
        XCTAssertTrue(app.buttons["task.attach.camera"].exists)
        XCTAssertTrue(app.buttons["task.attach.file"].exists)
        captureScreen(app, named: "Photo video camera and file attachment menu")
        app.buttons["task.attach.camera"].tap()
        XCTAssertTrue(app.staticTexts["この端末ではカメラを利用できません。"].waitUntilExists(timeout: 5))
        selectPhotos([.photo, .video], in: app)
        captureScreen(app, named: "Photo and video selected together")
        confirmPhotoSelection(in: app)
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        let attached = expectation(for: NSPredicate(format: "count == 2"), evaluatedWith: removals)
        wait(for: [attached], timeout: 60)
        let prompt = app.descendants(matching: .any)["task.message"]
        prompt.tap(); prompt.typeText("[success] Read selected media")
        app.buttons["task.send"].tap()
        let photo = app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image.")).firstMatch
        XCTAssertTrue(photo.waitUntilExists(timeout: 30), "Uploaded image did not decode in the conversation")
        XCTAssertTrue(app.textViews.matching(NSPredicate(
            format: "identifier == %@ AND value CONTAINS %@", "message.user-text", "添付:"
        )).firstMatch
            .waitUntilExists(timeout: 20))
        XCTAssertEqual(removals.count, 0)
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 30))
        let completed = expectation(
            for: NSPredicate(format: "label CONTAINS %@", "件の過去のメッセージ"),
            evaluatedWith: prefixedButton(app, prefix: "turn.activity.fixture-turn-")
        )
        wait(for: [completed], timeout: 10)
        XCTAssertFalse(prefixedButton(app, prefix: "turn.interrupt.").exists)
        XCTAssertFalse(app.staticTexts["notice"].exists, "Completed attachment send must not leave a Host error")
        captureScreen(app, named: "Uploaded photo and video in chat history")
    }

    func testSimulatorRetriesPhotoUploadAfterStorageRecovery() throws {
        let app = try connectedSimulatorApp(expandProject: false)
        try useSimulatorListFixture("worktree-conversation/upload")
        app.buttons["tasks.menu"].tap(); app.buttons["tasks.refresh"].tap()
        expandSimulatorProject(app)
        let row = app.descendants(matching: .any)["tasks.row.codex:fixture-upload-thread"]
        XCTAssertTrue(row.waitUntilExists(timeout: 15)); row.tap()
        let prompt = app.textFields["task.message"]
        XCTAssertTrue(prompt.waitUntilExists(timeout: 10))
        prompt.tap(); prompt.typeText("[success] Retry selected photos")
        try simulatorFixture("uploads/unavailable")
        var unavailable = true
        addTeardownBlock {
            if unavailable {
                try self.simulatorFixture("uploads/restore")
            }
        }
        selectPhotos([.photo, .video], in: app)
        confirmPhotoSelection(in: app)
        let notices = app.staticTexts.matching(identifier: "notice")
        XCTAssertTrue(notices.firstMatch.waitUntilExists(timeout: 30))
        let attach = app.buttons["task.attach"]
        let recovered = expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: attach)
        wait(for: [recovered], timeout: 10)
        let removals = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "を外す"))
        XCTAssertEqual(removals.count, 0)
        XCTAssertEqual(prompt.value as? String, "[success] Retry selected photos")
        try simulatorFixture("uploads/restore"); unavailable = false
        selectPhotos([.photo, .video], in: app)
        confirmPhotoSelection(in: app)
        let attached = expectation(for: NSPredicate(format: "count == 2"), evaluatedWith: removals)
        wait(for: [attached], timeout: 60)
        XCTAssertEqual(notices.count, 0)
        app.buttons["task.send"].tap()
        let final = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(final.waitUntilExists(timeout: 30))
        let finalID = final.identifier
        XCTAssertEqual(removals.count, 0)
        XCTAssertNotEqual(prompt.value as? String, "[success] Retry selected photos")
        app.terminate(); _ = try connectedSimulatorApp()
        XCTAssertTrue(row.waitUntilExists(timeout: 15)); row.tap()
        XCTAssertTrue(app.descendants(matching: .any)[finalID].waitUntilExists(timeout: 20))
        XCTAssertTrue(app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image."))
            .firstMatch.waitUntilExists(timeout: 15))
        XCTAssertEqual(notices.count, 0)
        captureScreen(app, named: "Recovered media upload survives reopening")
    }

    private enum PickerMedia {
        case photo, video

        var labels: (String, String) {
            switch self {
            case .photo: ("写真", "Photo")
            case .video: ("ビデオ", "Video")
            }
        }
    }

    private func selectPhotos(_ kinds: [PickerMedia], in app: XCUIApplication) {
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.photos"].tap()
        for kind in kinds {
            // The system picker follows the Simulator language: Japanese locally, English on CI runners.
            let (japanese, english) = kind.labels
            let media = app.images
                .matching(NSPredicate(format: "label CONTAINS %@ OR label CONTAINS %@", japanese, english)).firstMatch
            XCTAssertTrue(media.waitUntilExists(timeout: 15), app.debugDescription)
            media.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
    }

    private func confirmPhotoSelection(in app: XCUIApplication) {
        let confirm = app.navigationBars.buttons.matching(
            NSPredicate(format: "label IN %@", ["Add", "Done", "追加", "完了"])
        ).firstMatch
        XCTAssertTrue(confirm.waitUntilExists(timeout: 10))
        let enabled = expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: confirm)
        wait(for: [enabled], timeout: 10)
        confirm.tap()
    }

    private func selectAttachmentFixture(in app: XCUIApplication) {
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.file"].tap()
        let attachment = app.cells["attachment-fixture, txt"]
        // The picker restores the app's Documents directory asynchronously.
        // Wait for that destination before navigating a transient Browse page.
        if !attachment.waitUntilExists(timeout: 10) {
            let browse = app.buttons.matching(NSPredicate(format: "label IN %@", ["ブラウズ", "Browse"])).firstMatch
            if browse.waitUntilExists(timeout: 3) {
                browse.tap()
            }
            let onPhone = app.staticTexts.matching(
                NSPredicate(format: "label IN %@", ["このiPhone内", "On My iPhone"])
            ).firstMatch
            if onPhone.waitUntilExists(timeout: 3) {
                onPhone.tap()
            }
            let folder = app.staticTexts["bex"]
            if folder.waitUntilExists(timeout: 3) {
                folder.tap()
            }
        }
        XCTAssertTrue(attachment.waitUntilExists(timeout: 10), app.debugDescription)
        attachment.tap()
    }

    func testSimulatorCanAttachDownloadAndPrepareAIEdit() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Prepare file operations")
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").waitUntilExists(timeout: 20))
        selectAttachmentFixture(in: app)
        XCTAssertTrue(app.staticTexts["attachment-fixture.txt"].waitUntilExists(timeout: 15))
        app.terminate()
        let reopened = try connectedSimulatorApp()
        let row = prefixedElement(reopened, prefix: "tasks.row.codex:bex-task:")
        XCTAssertTrue(row.waitUntilExists(timeout: 10)); row.tap()
        XCTAssertTrue(reopened.staticTexts["attachment-fixture.txt"].waitUntilExists(timeout: 10))
        let message = reopened.descendants(matching: .any)["task.message"]
        message.tap(); message.typeText("Read the attached file")
        reopened.buttons["task.send"].tap()
        let historyAttachment = reopened.textViews.matching(NSPredicate(
            format: "identifier == %@ AND value CONTAINS %@",
            "message.user-text", "添付: attachment-fixture.txt"
        )).firstMatch
        XCTAssertTrue(historyAttachment.waitUntilExists(timeout: 15))
        openFiles(reopened)
        let file = reopened.buttons["file.hello.txt"]
        XCTAssertTrue(file.waitUntilExists(timeout: 10)); file.tap()
        XCTAssertTrue(reopened.textViews["file.editor"].waitUntilExists(timeout: 10))
        reopened.buttons["ダウンロード"].tap()
        let shareName = reopened.otherElements["LP.CaptionBar.TopCaption"]
        XCTAssertTrue(shareName.waitUntilExists(timeout: 15), reopened.debugDescription)
        XCTAssertEqual(shareName.label, "hello")
        let screenshot = XCTAttachment(screenshot: reopened.screenshot())
        screenshot.name = "Downloaded file in native share sheet"; screenshot.lifetime = .keepAlways; add(screenshot)
        reopened.buttons["header.closeButton"].tap()
        // Sharing returns to the file it shared.
        XCTAssertTrue(reopened.textViews["file.editor"].waitUntilExists(timeout: 10))
        XCTAssertTrue(reopened.buttons["file.ai-edit"].waitUntilExists(timeout: 10))
        reopened.buttons["file.ai-edit"].tap()
        XCTAssertTrue(message.waitUntilExists(timeout: 10))
        XCTAssertTrue((message.value as? String ?? "").contains("hello.txt"))
    }
}
