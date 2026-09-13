import Foundation
import XCTest

/// XCTest selectors remain on BexLaunchUITests for the fixture runner.
extension BexLaunchUITests {
    func testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[generated-images] Show generated images and links")
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 25))
        let finalID = finalAnswer.identifier
        func showGeneratedImage(_ name: String) {
            let image = app.images.matching(NSPredicate(
                format: "identifier BEGINSWITH %@",
                "message.image.fixture-generated-\(name)-"
            )).firstMatch
            for _ in 0 ..< 10 {
                if image.exists, image.isHittable {
                    break
                }
                app.swipeDown()
            }
            XCTAssertTrue(image.waitForExistence(timeout: 10), "Generated image did not decode")
            XCTAssertTrue(image.isHittable)
        }
        func openLink(_ label: String) {
            let link = app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
            for _ in 0 ..< 12 {
                if link.exists, link.isHittable {
                    break
                }
                app.swipeUp()
            }
            XCTAssertTrue(link.waitForExistence(timeout: 10)); link.tap()
            let done = app.buttons["conversation.preview.close"]
            XCTAssertTrue(done.waitForExistence(timeout: 15), "The Host file did not open in Quick Look")
            let screenshot = XCTAttachment(screenshot: app.screenshot())
            screenshot.name = label; screenshot.lifetime = .keepAlways; add(screenshot)
            done.tap()
        }
        showGeneratedImage("inline")
        showGeneratedImage("saved")
        captureScreen(app, named: "Generated image outside collapsed work")
        openLink("生成画像を開く")
        openLink("ファイルを開く")
        app.terminate(); _ = try connectedSimulatorApp()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 30))
        let number = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(number)"]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        showGeneratedImage("inline")
        showGeneratedImage("saved")
        openLink("生成画像を開く")
    }

    func testSimulatorBrowsesAllSessionImagesAndSavesTheSelection() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[generated-images] [gallery] Browse all generated images")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        let image = app.images.matching(NSPredicate(
            format: "identifier BEGINSWITH %@",
            "message.image.fixture-generated-inline-"
        )).firstMatch
        for _ in 0 ..< 10 {
            if image.exists, image.isHittable {
                break
            }
            app.swipeDown()
        }
        XCTAssertTrue(image.waitForExistence(timeout: 10)); image.tap()
        let position = app.staticTexts["conversation.preview.position"]
        let complete = expectation(for: NSPredicate(format: "label == %@", "8 / 8"), evaluatedWith: position)
        wait(for: [complete], timeout: 30)
        let first = app.images["conversation.preview.thumbnail.0"]
        XCTAssertTrue(first.waitForExistence(timeout: 10)); first.tap()
        XCTAssertEqual(position.label, "1 / 8")
        let save = app.buttons["conversation.preview.save"]
        let ready = expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: save)
        wait(for: [ready], timeout: 15)
        captureScreen(app, named: "Session image gallery including older turns and item gaps")
        save.tap()
        let saved = expectation(
            for: NSPredicate(format: "label == %@ AND enabled == false", "保存済み"),
            evaluatedWith: save
        )
        wait(for: [saved], timeout: 20)
        app.images["conversation.preview.thumbnail.1"].tap()
        XCTAssertEqual(position.label, "2 / 8")
        let next = expectation(for: NSPredicate(format: "label == %@ AND enabled == true", "保存"), evaluatedWith: save)
        wait(for: [next], timeout: 15)
        app.buttons["conversation.preview.close"].tap()
        XCTAssertTrue(image.waitForExistence(timeout: 10))
    }

    func testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[images] Display the attached images")
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitForExistence(timeout: 25))
        let finalID = finalAnswer.identifier
        let messageImage = app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image."))
            .firstMatch
        let hostImage = app.images["Hostから読み込んだ画像"]
        let inlineImage = app.images["インライン画像"]
        showImage(inlineImage, in: app, upward: true)
        inlineImage.tap()
        let save = app.buttons["conversation.preview.save"]
        let close = app.buttons["conversation.preview.close"]
        XCTAssertTrue(close.waitForExistence(timeout: 10))
        XCTAssertTrue(save.exists)
        XCTAssertLessThan(save.frame.midX, close.frame.midX)
        XCTAssertGreaterThan(save.frame.midX, app.frame.midX)
        captureScreen(app, named: "Expanded image with Save and Close at top right")
        close.tap()
        XCTAssertTrue(inlineImage.waitForExistence(timeout: 5))
        captureScreen(app, named: "Markdown image decoded in the conversation")
        showImage(hostImage, in: app, upward: false)
        showImage(messageImage, in: app, upward: false)
        captureScreen(app, named: "Host attachment decoded in the user message")
        app.terminate(); _ = try connectedSimulatorApp()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitForExistence(timeout: 30))
        let threadNumber = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = app.descendants(matching: .any)["tasks.row.fixture-thread-\(threadNumber)"]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        showImage(inlineImage, in: app, upward: true)
        showImage(hostImage, in: app, upward: false)
        showImage(messageImage, in: app, upward: false)
    }

    func showImage(_ image: XCUIElement, in app: XCUIApplication, upward: Bool) {
        for _ in 0 ..< 12 {
            if image.exists, image.isHittable {
                return
            }
            if upward {
                app.swipeUp()
            } else {
                app.swipeDown()
            }
        }
        XCTAssertTrue(image.waitForExistence(timeout: 10))
        XCTAssertTrue(image.isHittable)
    }
}
