import XCTest
@testable import Kallisti

final class ToolNameSanitizerTests: XCTestCase {
    func testBareToolIdentifierPassesThrough() {
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "web_search"), "web_search")
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "image-generate"), "image-generate")
    }

    func testShellCommandsNeverLeak() {
        let leak = "sed -n '340,420p' /home/user/project/connector/src/x.py"
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: leak), "Running a command")
        XCTAssertFalse(ToolNameSanitizer.displayLabel(for: leak).contains("/home"))
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "cat foo | grep bar"), "Running a command")
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "python3 -c 'print(1)'"), "Running a command")
    }

    func testFriendlyVerbsForKnownReadOnlyTools() {
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "view_file"), "Reading a file")
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "search"), "Searching")
    }

    func testOverlongIdentifierTruncated() {
        let long = String(repeating: "a", count: 40)
        let out = ToolNameSanitizer.displayLabel(for: long)
        XCTAssertLessThanOrEqual(out.count, 26)
        XCTAssertTrue(out.hasSuffix("\u{2026}"))
    }

    func testWhitespaceAndEmptyFallBack() {
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: "   "), "Working")
        XCTAssertEqual(ToolNameSanitizer.displayLabel(for: ""), "Working")
    }
}
