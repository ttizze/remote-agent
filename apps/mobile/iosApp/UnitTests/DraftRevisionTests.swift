@testable import BexNativeState
import XCTest

final class DraftRevisionTests: XCTestCase {
    func testOlderReceiptDoesNotUnlockNewerEdit() {
        var edits = DraftRevision()
        let first = edits.edit("a")
        let second = edits.edit("ab")
        XCTAssertEqual(second.base, "a")
        XCTAssertFalse(edits.acknowledge(first.revision))
        XCTAssertEqual(edits.pending, second.revision)
        XCTAssertTrue(edits.acknowledge(second.revision))
        XCTAssertNil(edits.pending)
    }

    func testHostSwitchRejectsOldReceipt() {
        var edits = DraftRevision()
        let old = edits.edit("old host")
        edits.reset()
        let new = edits.edit("new host")
        XCTAssertFalse(edits.acknowledge(old.revision))
        XCTAssertEqual(edits.pending, new.revision)
    }
}
