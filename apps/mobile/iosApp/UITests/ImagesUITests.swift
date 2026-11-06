import CoreGraphics
import Foundation
import XCTest

private enum WorkspacePreview {
    case quickLook, media, web, download, truncated, unavailable
    case text(String)
}

private func renderedTextContrast(_ image: CGImage, region: CGRect) -> Int {
    guard let sample = image.cropping(to: region),
          let context = CGContext(data: nil, width: 128, height: 128, bitsPerComponent: 8, bytesPerRow: 128,
                                  space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue)
    else { return 0 }
    context.draw(sample, in: CGRect(x: 0, y: 0, width: 128, height: 128))
    guard let pixels = context.makeImage()?.dataProvider?.data else { return 0 }
    let values = pixels as Data
    return Int(values.max() ?? 0) - Int(values.min() ?? 0)
}

/// XCTest selectors remain on LaunchUITests for the fixture runner.
extension LaunchUITests {
    func testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[generated-images] Show generated images and links")
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitUntilExists(timeout: 25))
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
            XCTAssertTrue(image.waitUntilExists(timeout: 10), "Generated image did not decode")
            XCTAssertTrue(image.isHittable)
            XCTAssertFalse(app.staticTexts["生成画像"].exists, "Generated images must not render a redundant caption")
        }
        showGeneratedImage("inline")
        showGeneratedImage("saved")
        captureScreen(app, named: "Generated image outside collapsed work")
        verifyWorkspaceFileLink(app, index: 0, filename: "fixture image.png")
        verifyWorkspaceFileLink(app, index: 1, filename: "hello.txt", expected: .text("日本語のファイル"))
        for (offset, fixture) in [
            ("video.mp4", WorkspacePreview.media), ("audio.wav", .media),
            ("animation.gif", .quickLook), ("vector.svg", .web), ("document.pdf", .quickLook),
            ("archive.zip", .download), ("large.txt", .truncated), ("unknown.bin", .unavailable)
        ].enumerated() {
            verifyWorkspaceFileLink(app, index: offset + 3, filename: "fixture \(fixture.0)", expected: fixture.1)
        }
        app.terminate(); _ = try connectedSimulatorApp()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 30))
        let number = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(number)")]
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        showGeneratedImage("inline")
        showGeneratedImage("saved")
        verifyWorkspaceFileLink(app, index: 0, filename: "fixture image.png")
    }

    private func verifyWorkspaceFileLink(
        _ app: XCUIApplication, index: Int, filename: String, expected: WorkspacePreview = .quickLook
    ) {
        // File chips replace the authored link captions. The fixture lists the image, text file, then website.
        let link = prefixedElement(app, prefix: "item.fixture-final-").links.element(boundBy: index)
        showConversationElement(link, in: app, upward: true)
        link.tap()
        let done = app.buttons["file.close"]
        XCTAssertTrue(done.waitUntilExists(timeout: 15), "The Host file did not open")
        XCTAssertTrue(app.navigationBars[filename].exists)
        verifyWorkspacePreview(app, expected: expected)
        let screen = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        screen.name = filename
        screen.lifetime = .keepAlways
        add(screen)
        done.tap()
        wait(for: [expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: done)], timeout: 5)
        tapWhenStable(app.navigationBars.buttons["閉じる"])
        wait(for: [expectation(for: NSPredicate(format: "isHittable == true"),
                               evaluatedWith: app.textFields["task.message"])], timeout: 10)
    }

    private func verifyWorkspacePreview(_ app: XCUIApplication, expected: WorkspacePreview) {
        switch expected {
        case let .text(text):
            let editor = app.textViews["file.editor"]
            XCTAssertTrue(editor.exists)
            wait(
                for: [expectation(for: NSPredicate(format: "value CONTAINS %@", text), evaluatedWith: editor)],
                timeout: 10
            )
        case .truncated:
            let preview = app.textViews["file.preview.text"]
            XCTAssertTrue(preview.waitUntilExists(timeout: 15))
            XCTAssertTrue((preview.value as? String)?.hasPrefix("Large text fixture") == true)
            XCTAssertFalse((preview.value as? String)?.contains("END OF FULL FILE") == true)
            XCTAssertFalse(app.buttons["file.save"].isEnabled)
            XCTAssertTrue(app.buttons["file.download"].exists)
            XCTAssertFalse(app.textViews["file.editor"].exists)
            let image = preview.screenshot().image.cgImage
            let scale = CGFloat(image?.width ?? 0) / preview.frame.width
            // Sample below the heading, away from the scroll indicator.
            let area = CGRect(x: 12, y: 48, width: 100, height: 100)
                .applying(CGAffineTransform(scaleX: scale, y: scale))
            let contrast = image.map { renderedTextContrast($0, region: area) } ?? 0
            XCTAssertGreaterThan(contrast, 128, "The long unbroken line must actually be drawn")
        case .unavailable:
            XCTAssertTrue(app.descendants(matching: .any)["file.error"].waitUntilExists(timeout: 15))
            XCTAssertTrue(app.buttons["file.download"].exists)
            XCTAssertFalse(app.buttons["file.save"].isEnabled)
        default:
            verifyNativeWorkspacePreview(app, expected: expected)
        }
    }

    private func verifyNativeWorkspacePreview(_ app: XCUIApplication, expected: WorkspacePreview) {
        XCTAssertTrue(app.buttons["file.share"].waitUntilExists(timeout: 15))
        XCTAssertFalse(app.textViews["file.editor"].exists, "Binary files must bypass the text editor")
        XCTAssertTrue(app.buttons["file.reload"].exists)
        XCTAssertTrue(app.buttons["file.ai-edit"].exists)
        switch expected {
        case .quickLook:
            let preview = app.descendants(matching: .any)["file.preview"]
            XCTAssertTrue(preview.waitUntilExists(timeout: 15))
            XCTAssertTrue(preview.isHittable)
            XCTAssertFalse(app.staticTexts["このファイル形式はプレビューできません"].exists)
        case .media:
            XCTAssertTrue(app.otherElements["file.preview.media"].waitUntilExists(timeout: 15))
            verifyMediaPlayback(app)
        case .web:
            let content = app.webViews.firstMatch
            XCTAssertTrue(content.waitUntilExists(timeout: 15))
            XCTAssertTrue(content.isHittable)
            XCTAssertGreaterThan(content.frame.height, 100)
        default:
            break
        }
    }

    private func verifyMediaPlayback(_ app: XCUIApplication) {
        let play = app.buttons.matching(NSPredicate(format: "label IN %@", ["Play", "再生"])).firstMatch
        if play.exists {
            play.tap()
        } else {
            app.otherElements["file.preview.media"].tap()
        }
        let toggle = app.buttons["Play/Pause"]
        wait(for: [expectation(for: NSPredicate(format: "label IN %@", ["Pause", "一時停止"]),
                               evaluatedWith: toggle)], timeout: 15)
        toggle.tap()
        wait(for: [expectation(for: NSPredicate(format: "label IN %@", ["Play", "再生"]),
                               evaluatedWith: toggle)], timeout: 5)
    }

    func testSimulatorOpensOnlyTheTappedImageAndSavesIt() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[generated-images] [gallery] Open only the tapped image")
        XCTAssertTrue(prefixedElement(app, prefix: "item.fixture-final-").waitUntilExists(timeout: 25))
        let image = app.images.matching(NSPredicate(
            format: "identifier BEGINSWITH %@",
            "message.image.fixture-generated-inline-"
        )).firstMatch
        showConversationElement(image, in: app, upward: false)
        image.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        // QuickLook zooms only the tapped image; the native toolbar shares or saves it.
        let done = app.buttons["conversation.preview.close"]
        XCTAssertTrue(done.waitUntilExists(timeout: 10))
        app.windows.firstMatch.pinch(withScale: 2, velocity: 1)
        captureScreen(app, named: "Only the tapped image opens even in a conversation with many images")
        app.buttons["conversation.preview.share"].tap()
        let save = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label IN %@", ["Save Image", "画像を保存"])).firstMatch
        XCTAssertTrue(save.waitUntilExists(timeout: 10))
        save.tap()
        wait(for: [expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: save)], timeout: 20)
        done.tap()
        XCTAssertTrue(image.waitUntilExists(timeout: 10))
    }

    func testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening() throws {
        let app = try connectedSimulatorApp()
        try startSimulatorConversation(app, promptText: "[images] Display the attached images")
        let finalAnswer = prefixedElement(app, prefix: "item.fixture-final-")
        XCTAssertTrue(finalAnswer.waitUntilExists(timeout: 25))
        let finalID = finalAnswer.identifier
        let messageImage = app.images.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message.image."))
            .firstMatch
        let hostImage = app.images["Hostから読み込んだ画像"]
        let inlineImage = app.images["インライン画像"]
        showConversationElement(inlineImage, in: app, upward: true)
        inlineImage.tap()
        let done = app.buttons["conversation.preview.close"]
        XCTAssertTrue(done.waitUntilExists(timeout: 10))
        XCTAssertTrue(app.buttons["conversation.preview.share"].exists)
        captureScreen(app, named: "Expanded image in QuickLook")
        done.tap()
        XCTAssertTrue(inlineImage.waitUntilExists(timeout: 5))
        captureScreen(app, named: "Markdown image decoded in the conversation")
        showConversationElement(hostImage, in: app, upward: false)
        showConversationElement(messageImage, in: app, upward: false)
        captureScreen(app, named: "Host attachment decoded in the user message")
        app.terminate(); _ = try connectedSimulatorApp()
        XCTAssertTrue(app.descendants(matching: .any)["tasks.list"].waitUntilExists(timeout: 30))
        let threadNumber = try XCTUnwrap(finalID.split(separator: "-").dropLast().last)
        let row = try app
            .descendants(matching: .any)[simulatorTaskRowIdentifier("session-identity/fixture-thread-\(threadNumber)")]
        XCTAssertTrue(row.waitUntilExists(timeout: 20)); row.tap()
        showConversationElement(inlineImage, in: app, upward: true)
        showConversationElement(hostImage, in: app, upward: false)
        showConversationElement(messageImage, in: app, upward: false)
    }

    func showConversationElement(_ element: XCUIElement, in app: XCUIApplication, upward: Bool) {
        let composer = app.buttons["task.diff"].exists ? app.buttons["task.diff"] : app.textFields["task.message"]
        let visible = (app.navigationBars.firstMatch.frame.maxY + 8) ..< (composer.frame.minY - 16)
        for _ in 0 ..< 15 {
            if element.exists, element.isHittable,
               visible.contains(element.frame.midY) {
                return
            }
            let below = element.exists ? element.frame.midY >= visible.upperBound : upward
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: below ? 0.55 : 0.35))
                .press(forDuration: 0.05, thenDragTo: app.coordinate(
                    withNormalizedOffset: CGVector(dx: 0.5, dy: below ? 0.35 : 0.55)
                ))
        }
        XCTAssertTrue(element.waitUntilExists(timeout: 10))
        XCTAssertTrue(element.isHittable)
        XCTAssertGreaterThanOrEqual(element.frame.midY, visible.lowerBound)
        XCTAssertLessThan(element.frame.midY, visible.upperBound)
    }
}
