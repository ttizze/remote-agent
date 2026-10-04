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
            XCTAssertFalse(app.staticTexts["生成画像"].exists, "Generated images must not render a redundant caption")
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
        let row = try app.descendants(matching: .any)[simulatorConversationRow("fixture-thread-\(number)")]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        showGeneratedImage("inline")
        showGeneratedImage("saved")
        openLink("生成画像を開く")
    }

    func testSimulatorOpensOnlyTheTappedImageAndSavesIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[generated-images] [gallery] Open only the tapped image")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitForExistence(timeout: 25))
        let image = app.images.matching(NSPredicate(
            format: "identifier BEGINSWITH %@",
            "message.image.fixture-generated-inline-"
        )).firstMatch
        showImage(image, in: app, upward: false)
        image.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertTrue(app.buttons["conversation.preview.close"].waitForExistence(timeout: 10))
        let preview = app.images["conversation.preview.image"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        XCTAssertTrue(preview.isHittable)
        preview.pinch(withScale: 2, velocity: 1)
        XCTAssertFalse(app.staticTexts["conversation.preview.position"].exists)
        let save = app.buttons["conversation.preview.save"]
        let ready = expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: save)
        wait(for: [ready], timeout: 15)
        captureScreen(app, named: "Only the tapped image opens even in a conversation with many images")
        save.tap()
        let saved = expectation(
            for: NSPredicate(format: "label == %@ AND enabled == false", "保存済み"),
            evaluatedWith: save
        )
        wait(for: [saved], timeout: 20)
        app.swipeLeft()
        app.swipeRight()
        XCTAssertEqual(save.label, "保存済み")
        XCTAssertFalse(save.isEnabled)
        XCTAssertFalse(app.staticTexts["conversation.preview.position"].exists)
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
        XCTAssertTrue(app.images["conversation.preview.image"].waitForExistence(timeout: 10))
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
        let row = try app.descendants(matching: .any)[simulatorConversationRow("fixture-thread-\(threadNumber)")]
        XCTAssertTrue(row.waitForExistence(timeout: 20)); row.tap()
        showImage(inlineImage, in: app, upward: true)
        showImage(hostImage, in: app, upward: false)
        showImage(messageImage, in: app, upward: false)
    }

    func showImage(_ image: XCUIElement, in app: XCUIApplication, upward: Bool) {
        for _ in 0 ..< 15 {
            if image.exists, image.isHittable,
               image.frame.midY > app.frame.height * 0.2,
               image.frame.midY < app.frame.height * 0.65 {
                return
            }
            let below = image.exists ? image.frame.midY > app.frame.height * 0.65 : upward
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: below ? 0.55 : 0.35))
                .press(forDuration: 0.05, thenDragTo: app.coordinate(
                    withNormalizedOffset: CGVector(dx: 0.5, dy: below ? 0.35 : 0.55)
                ))
        }
        XCTAssertTrue(image.waitForExistence(timeout: 10))
        XCTAssertTrue(image.isHittable)
        XCTAssertGreaterThan(image.frame.midY, app.frame.height * 0.2)
        XCTAssertLessThan(image.frame.midY, app.frame.height * 0.65)
    }
}
