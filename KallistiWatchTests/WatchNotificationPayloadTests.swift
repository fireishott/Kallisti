import XCTest

/// The payload decoder is the watch's only input, so its two jobs are pinned here:
/// a good payload decodes, and a hostile string cannot reach a label unshrunk.
final class WatchNotificationPayloadTests: XCTestCase {
    func testASessionPayloadDecodes() {
        let payload = WatchNotificationPayload.decode(from: [
            "category": "session.ready",
            "sessionId": "20261114_101010_abcdef",
            "title": "Answer ready",
        ])
        XCTAssertEqual(payload?.sessionId, "20261114_101010_abcdef")
        XCTAssertEqual(payload?.title, "Answer ready")
    }

    func testAnUnknownCategoryIsIgnored() {
        XCTAssertNil(
            WatchNotificationPayload.decode(from: ["category": "nothing.here"])
        )
    }

    func testSanitizeCollapsesWhitespaceAndClips() {
        let long = String(repeating: "a b ", count: 200)
        let clean = WatchNotificationPayload.sanitize(long)
        XCTAssertFalse(clean.contains("  "))
        XCTAssertLessThanOrEqual(clean.count, 200)
    }
}
