import XCTest
@testable import Kallisti

final class StartupUXTests: XCTestCase {
    func testPreReadyNativeLaunchShowsLoadingSurface() {
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: false, isRecovering: true,
            hasStoredLogin: true, isBootstrapping: false,
            hasTerminalLegacyFailure: false
        ))
    }

    func testStoredLoginReconnectShowsLoadingSurface() {
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: true,
            hasStoredLogin: true, isBootstrapping: false,
            hasTerminalLegacyFailure: false
        ))
    }

    func testConnectedNativeSessionHidesLoadingSurface() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: false,
            hasStoredLogin: true, isBootstrapping: false,
            hasTerminalLegacyFailure: false
        ))
    }

    func testTerminalLegacyFailureShowsRecoveryUI() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: false, isLaunchReady: true, isRecovering: false,
            hasStoredLogin: false, isBootstrapping: false,
            hasTerminalLegacyFailure: true
        ))
    }
}

final class ProfileDisplayNameTests: XCTestCase {
    func testSoleDefaultProfileUsesIgnyte() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "default", profileCount: 1), "Ignyte")
    }

    func testCachedDefaultBeforeCatalogLoadUsesIgnyte() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "default", profileCount: 0), "Ignyte")
    }

    func testMultipleProfilesKeepRoutingSlugVisible() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "default", profileCount: 2), "default")
    }

    func testNamedProfileKeepsItsName() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "work", profileCount: 1), "work")
    }
}
