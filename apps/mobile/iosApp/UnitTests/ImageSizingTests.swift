@testable import BexNativeState
import CoreGraphics
import XCTest

final class ImageSizingTests: XCTestCase {
    func testLargeImagesShrinkToTheLongestEdge() {
        XCTAssertEqual(ImageSizing.scaled(CGSize(width: 4032, height: 3024)), CGSize(width: 2048, height: 1536))
        XCTAssertEqual(ImageSizing.scaled(CGSize(width: 1000, height: 5000)), CGSize(width: 410, height: 2048))
    }

    func testSmallImagesKeepTheirSize() {
        XCTAssertEqual(ImageSizing.scaled(CGSize(width: 2048, height: 10)), CGSize(width: 2048, height: 10))
        XCTAssertEqual(ImageSizing.scaled(.zero), .zero)
    }

    func testOnlyAcceptedFormatsWithinTheLimitPassThrough() {
        XCTAssertFalse(ImageSizing.needsRendering(mimeType: "image/png", needsCompression: false))
        XCTAssertTrue(ImageSizing.needsRendering(mimeType: "image/png", needsCompression: true))
        XCTAssertTrue(ImageSizing.needsRendering(mimeType: "image/heic", needsCompression: false))
    }

    func testRenderedImagesAreNamedAsJpeg() {
        XCTAssertEqual(ImageSizing.jpegName("IMG_0001.HEIC"), "IMG_0001.jpg")
        XCTAssertEqual(ImageSizing.jpegName("archive.tar.heic"), "archive.tar.jpg")
        XCTAssertEqual(ImageSizing.jpegName("photo"), "photo.jpg")
    }
}
