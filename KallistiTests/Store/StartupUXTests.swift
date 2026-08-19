import XCTest
@testable import Kallisti

final class StartupUXTests: XCTestCase {
    func testPreReadyNativeLaunchShowsLoadingSurface() {
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: false, isRecovering: true,
            hasStoredLogin: true, isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        ))
    }

    func testStoredLoginReconnectShowsLoadingSurface() {
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: true,
            hasStoredLogin: true, isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: true
        ))
    }

    func testConnectedNativeSessionHidesLoadingSurface() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: false,
            hasStoredLogin: true, isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        ))
    }

    func testTerminalLegacyFailureShowsRecoveryUI() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: false, isLaunchReady: true, isRecovering: false,
            hasStoredLogin: false, isBootstrapping: false,
            hasTerminalLegacyFailure: true,
            reconnectDebounced: false
        ))
    }

    // MARK: - Build 76: cold-start onboarding splash glimpse regression
    //
    // Before build 76 the init Task that resolves hasStoredLogin ran on the
    // next MainActor hop AFTER AppRootView's first SwiftUI render. A
    // returning user with a stored login briefly saw hasStoredLogin=false
    // and the body mounted OnboardingFlowView for one frame, producing the
    // "splash glimpse" of onboarding before the chat UI replaced it. The
    // fix is hasResolvedStoredLogin on NativeKallistiClient, flipped true
    // by the init Task via defer, gating shouldShowLoadingSurface.

    /// Fresh install, init Task still in flight -> surface shown even though
    /// neither hasStoredLogin nor hasConfiguredRelay are set. This is the
    /// exact race: without the gate, the body would mount onboarding for one
    /// frame. Verified at the gate's two unguarded extents (unresolved + no
    /// stored login + no relay, AND unresolved + stored login + no relay).
    func testUnresolvedFreshInstallHidesOnboardingBehindLoadingSurface() {
        // No stored login, no configured relay, init not yet resolved.
        // WITHOUT the gate: false (body would mount onboarding -> splash).
        // WITH the gate: true (loading surface covers the body).
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: false, isRecovering: false,
            hasStoredLogin: false, hasConfiguredRelay: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false,
            hasResolvedStoredLogin: false
        ))
    }

    /// Returning user, init Task still in flight -> surface shown even
    /// though hasStoredLogin is true. This is the OTHER half of the race:
    /// without the gate, body would briefly skip AdaptiveRootView for
    /// OnboardingFlowView. With the gate, loading surface is the only
    /// thing mounted until the init Task flips hasResolvedStoredLogin true.
    func testUnresolvedReturningUserHidesOnboardingBehindLoadingSurface() {
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: false, isRecovering: false,
            hasStoredLogin: true, hasConfiguredRelay: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false,
            hasResolvedStoredLogin: false
        ))
    }

    /// Resolved fresh install, launch ready -> loading surface hidden so
    /// onboarding paints immediately. The spec calls this out as a positive
    /// gate result: "Fresh install must go LoadingSurface then onboarding
    /// after resolved false."
    func testResolvedFreshInstallLaunchReadyHidesLoadingSurface() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: false,
            hasStoredLogin: false, hasConfiguredRelay: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false,
            hasResolvedStoredLogin: true
        ))
    }

    /// Resolved returning user, launch ready -> loading surface hidden so
    /// AdaptiveRootView (chat UI) paints. The spec calls this out: "Returning
    /// user must go LoadingSurface then AdaptiveRootView." Without the
    /// legacy-mode terminal failure flag, native mode past isLaunchReady
    /// must NEVER re-show the surface.
    /// Regression: iOS can retain Keychain auth after the app is deleted,
    /// but the newly installed app has no saved relay in UserDefaults. That
    /// stale marker must not trap the user behind a reconnect splash; onboarding
    /// is the only valid first screen for the new app installation.
    func testResolvedFreshInstallWithStaleKeychainAuthHidesLoadingSurface() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: false, isRecovering: true,
            hasStoredLogin: true, hasConfiguredRelay: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false,
            hasResolvedStoredLogin: true
        ))
    }

    func testResolvedReturningUserLaunchReadyHidesLoadingSurface() {
        XCTAssertFalse(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: false,
            hasStoredLogin: true, hasConfiguredRelay: true,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false,
            hasResolvedStoredLogin: true
        ))
    }

    /// Build 114: a sustained reconnect covers stale chat until recovery.
    func testPostConnectReconnectShowsLoadingSurface() {
        XCTAssertTrue(AppRootView.shouldShowLoadingSurface(
            isNative: true, isLaunchReady: true, isRecovering: true,
            hasStoredLogin: true, hasConfiguredRelay: true,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: true,
            hasResolvedStoredLogin: true
        ))
    }
}

final class ProfileDisplayNameTests: XCTestCase {
    func testSoleDefaultProfileUsesKallisti() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "default", profileCount: 1), "Kallisti")
    }

    func testCachedDefaultBeforeCatalogLoadUsesKallisti() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "default", profileCount: 0), "Kallisti")
    }

    func testMultipleProfilesKeepRoutingSlugVisible() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "default", profileCount: 2), "default")
    }

    func testNamedProfileKeepsItsName() {
        XCTAssertEqual(ProfileStore.resolveDisplayName(activeProfileName: "work", profileCount: 1), "work")
    }
}
