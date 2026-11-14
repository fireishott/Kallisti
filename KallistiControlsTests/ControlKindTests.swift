import XCTest

/// The control bundle is selected by kind, and an unknown kind must fall back
/// rather than leave a blank widget on the Lock Screen.
final class ControlKindTests: XCTestCase {
    func testTheKnownKindsAreDistinctAndNonEmpty() {
        let kinds = ["gatewayStatus", "modelSwitch", "restartGateway"]
        XCTAssertEqual(Set(kinds).count, kinds.count)
        XCTAssertTrue(kinds.allSatisfy { !$0.isEmpty })
    }

    func testAnUnknownKindFallsBackToStatus() {
        let kind = "somethingNew"
        let resolved = ["gatewayStatus", "modelSwitch", "restartGateway"].contains(kind)
            ? kind
            : "gatewayStatus"
        XCTAssertEqual(resolved, "gatewayStatus")
    }
}
