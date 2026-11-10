#if os(macOS)
    import AppKit
    @testable import BexNshNative
    import NativeBrowserViews
    import XCTest

    private final class BrowserEvents {
        var values: [Data] = []
    }

    @MainActor final class BrowserTests: XCTestCase {
        func testRemotePageFitsBothTallAndWidePanelsAndCanBeMagnified() throws {
            let canvas = BrowserCanvasView()
            canvas.display(NSImage(size: NSSize(width: 1_200, height: 750)))
            let page = try XCTUnwrap(canvas.documentView)
            for size in [NSSize(width: 400, height: 800), NSSize(width: 800, height: 300)] {
                canvas.frame.size = size
                canvas.layoutSubtreeIfNeeded()
                let visible = canvas.documentVisibleRect
                XCTAssertGreaterThanOrEqual(visible.width + 1, page.bounds.width)
                XCTAssertGreaterThanOrEqual(visible.height + 1, page.bounds.height)
                let fitted = canvas.magnification
                canvas.setMagnification(fitted * 2, centeredAt: NSPoint(x: 600, y: 375))
                XCTAssertGreaterThan(canvas.magnification, fitted)
            }
        }

        func testMacBrowserUsesAppKitAndEmitsActionsOnlyWhileItsOwnerExists() throws {
            let events = BrowserEvents()
            let canvas: BrowserCanvasView
            do {
                let handle = browserCreate(Unmanaged.passUnretained(events).toOpaque()) { context, bytes, count in
                    guard let context, let bytes else { return }
                    let events = Unmanaged<BrowserEvents>.fromOpaque(context).takeUnretainedValue()
                    events.values.append(Data(bytes: bytes, count: count))
                }
                defer { browserRelease(handle) }
                let native = Unmanaged<NSView>.fromOpaque(browserView(handle)).takeUnretainedValue()
                canvas = try XCTUnwrap(native as? BrowserCanvasView)
                let page = try XCTUnwrap(canvas.documentView as? NSImageView)
                let bitmap = try XCTUnwrap(NSBitmapImageRep(
                    bitmapDataPlanes: nil, pixelsWide: 2, pixelsHigh: 2, bitsPerSample: 8,
                    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                    bytesPerRow: 0, bitsPerPixel: 0
                ))
                bitmap.bitmapData?.initialize(repeating: 128, count: bitmap.bytesPerRow * bitmap.pixelsHigh)
                let image = try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).base64EncodedString()
                func update(_ enabled: Bool) throws {
                    let data = try JSONSerialization.data(withJSONObject: ["image": image, "enabled": enabled])
                    data.withUnsafeBytes { bytes in
                        browserUpdate(handle, bytes.bindMemory(to: UInt8.self).baseAddress!, data.count)
                    }
                }
                try update(false)
                XCTAssertNotNil(page.image)
                XCTAssertTrue(canvas.allowsMagnification)
                XCTAssertTrue(canvas.hasVerticalScroller)
                XCTAssertTrue(canvas.hasHorizontalScroller)
                XCTAssertNil(canvas.send)
                try update(true)
                canvas.send?(.click(x: 7, y: 8))
                canvas.send?(.scroll(x: 7, y: 8, deltaX: -3, deltaY: 12))
                let click = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(events.values.first)) as? [String: [String: Double]])
                let scroll = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(events.values.dropFirst().first)) as? [String: [String: Double]])
                XCTAssertEqual(click, ["Click": ["x": 7, "y": 8]])
                XCTAssertEqual(scroll, ["Scroll": ["x": 7, "y": 8, "delta_x": -3, "delta_y": 12]])
            }
            canvas.send?(.click(x: 1, y: 2))
            XCTAssertEqual(events.values.count, 2)
        }
    }
#endif
