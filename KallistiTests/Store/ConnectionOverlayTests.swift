import Foundation
import Testing
@testable import Kallisti

/// Regression tests for AppRootView.shouldShowLoadingSurface.
///
/// The static function determines whether the full-screen loading / reconnecting
/// overlay should be visible.  These tests lock in the expected decision tree.
@Suite("Connection overlay logic")
struct ConnectionOverlayTests {

    // MARK: - Native path

    @Test("Native launch before ready always shows the loading surface")
    func nativeLaunchNotReady() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: true,
            isLaunchReady: false,
            isRecovering: true,
            hasStoredLogin: true,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        )
        #expect(show == true)
    }

    @Test("Native mid-session reconnect keeps the chat visible")
    func nativeReconnectDebounced() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: true,
            isLaunchReady: true,
            isRecovering: true,
            hasStoredLogin: true,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: true
        )
        #expect(show == false)
    }

    @Test("Native mid-session reconnect without debounce hides the loading surface")
    func nativeReconnectNotDebounced() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: true,
            isLaunchReady: true,
            isRecovering: true,
            hasStoredLogin: true,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        )
        #expect(show == false)
    }

    @Test("Native connected session hides the loading surface")
    func nativeConnected() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: true,
            isLaunchReady: true,
            isRecovering: false,
            hasStoredLogin: true,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        )
        #expect(show == false)
    }

    @Test("Native recovering without stored login hides the loading surface")
    func nativeRecoveringNoStoredLogin() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: true,
            isLaunchReady: true,
            isRecovering: true,
            hasStoredLogin: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: true
        )
        #expect(show == false)
    }

    // MARK: - Legacy / non-native path

    @Test("Non-native not ready and not bootstrapping shows the loading surface")
    func legacyNotReady() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: false,
            isLaunchReady: false,
            isRecovering: false,
            hasStoredLogin: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        )
        #expect(show == true)
    }

    @Test("Non-native bootstrapping shows the loading surface")
    func legacyBootstrapping() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: false,
            isLaunchReady: true,
            isRecovering: false,
            hasStoredLogin: false,
            isBootstrapping: true,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        )
        #expect(show == true)
    }

    @Test("Non-native with terminal legacy failure hides the loading surface")
    func legacyTerminalFailure() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: false,
            isLaunchReady: true,
            isRecovering: false,
            hasStoredLogin: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: true,
            reconnectDebounced: false
        )
        #expect(show == false)
    }

    @Test("Non-native ready and not bootstrapping hides the loading surface")
    func legacyReadyNoBootstrap() {
        let show = AppRootView.shouldShowLoadingSurface(
            isNative: false,
            isLaunchReady: true,
            isRecovering: false,
            hasStoredLogin: false,
            isBootstrapping: false,
            hasTerminalLegacyFailure: false,
            reconnectDebounced: false
        )
        #expect(show == false)
    }
}
