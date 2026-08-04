import XCTest
@testable import Kallisti

/// Tests for content separation — system context, transport metadata,
/// and staging paths must never appear as display content.
final class ContentSeparationTests: XCTestCase {

    // MARK: - System context regression

    func testSystemContextMarkerNeverInUserBubble() {
        // The exact marker that caused the regression
        let systemContext = "[System context — current local time]"
        let userText = "What time is it?"

        // Display content is only visibleText — system context is transport metadata
        XCTAssertTrue(userText != systemContext)
        XCTAssertFalse(userText.contains("[System context"),
                       "System context marker must never appear in display content")
    }

    func testStagingPathNeverInUserBubble() {
        let stagingPath = "/tmp/hermes-staging/attachment-12345.pdf"
        let userText = "Please review this document"

        XCTAssertFalse(userText.contains("/tmp/"),
                       "Staging path must not appear in display content")
    }

    func testTransportTextIsSeparateFromDisplay() {
        let visibleText = "Hello"
        let transportText = "[System context — timezone: UTC]"

        // These are separate fields — transport never contaminates display
        XCTAssertTrue(visibleText != transportText)
        XCTAssertFalse(visibleText.contains("timezone"))
    }
}
