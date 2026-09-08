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
        for count in 1 ... 2 {
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
        captureScreen(app, named: "Two separately selected photos staged")
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
        captureScreen(app, named: "Photo video camera and file attachment menu")
        app.buttons["task.attach.camera"].tap()
        XCTAssertTrue(app.staticTexts["この端末ではカメラを利用できません。"].waitForExistence(timeout: 5))
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.photos"].tap()
        for label in ["写真", "ビデオ"] {
            let media = app.images.matching(NSPredicate(format: "label CONTAINS %@", label)).firstMatch
            XCTAssertTrue(media.waitForExistence(timeout: 15), app.debugDescription)
            media.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
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
        captureScreen(app, named: "Uploaded photo and video in chat history")
    }

    func testSimulatorCanAttachDownloadAndPrepareAIEdit() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[success] Prepare file operations")
        XCTAssertTrue(prefixedButton(app, prefix: "turn.activity.fixture-turn-").waitForExistence(timeout: 20))
        app.buttons["task.attach"].tap()
        app.buttons["task.attach.file"].tap()
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
