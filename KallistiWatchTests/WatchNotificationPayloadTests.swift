import XCTest

/// The payload decoder is the watch's only input, so its two jobs are pinned here:
/// a good payload decodes, and a hostile string cannot reach a label unshrunk.
final class WatchNotificationPayloadTests: XCTestCase {
    func testAReadyPayloadDecodes() {
        let conversation = UUID()
        let payload = WatchNotificationPayload.decode(from: [
            NotificationPayloadKey.category.rawValue:
                NotificationCategoryID.messageReady.rawValue,
            NotificationPayloadKey.conversationId.rawValue:
                conversation.uuidString.lowercased(),
            NotificationPayloadKey.contractVersion.rawValue:
                NotificationContractVersion.current,
            NotificationPayloadKey.sanitizedPreview.rawValue: "The answer is ready.",
            NotificationPayloadKey.title.rawValue: "Answer ready",
        ])
        XCTAssertEqual(payload?.conversationId, conversation)
        XCTAssertEqual(payload?.title, "Answer ready")
        XCTAssertEqual(payload?.sanitizedPreview, "The answer is ready.")
        XCTAssertFalse(payload?.id.isEmpty ?? true)
    }

    func testAnUnknownCategoryIsIgnored() {
        XCTAssertNil(
            WatchNotificationPayload.decode(from: [
                NotificationPayloadKey.category.rawValue: "nothing.here"
            ])
        )
    }

    func testSanitizeCollapsesWhitespaceAndClips() {
        let long = String(repeating: "a b ", count: 200)
        let clean = WatchNotificationPayload.sanitize(long)
        XCTAssertFalse(clean.contains("  "))
        XCTAssertLessThanOrEqual(clean.count, 200)
    }
}
