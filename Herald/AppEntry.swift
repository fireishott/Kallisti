import SwiftUI
import UIKit
import UserNotifications

// MARK: - App delegate (Eris-only)
//
// The legacy connector/relay backend is gone. This delegate keeps only what a
// forward-looking Eris app still needs: the notification-center delegate so a
// banner renders in the foreground, and remote-notification registration (kept
// for future APNs use - Eris does not consume the token yet).

/// Carries a UIKit completion block across a concurrency hop.
///
/// `UNUserNotificationCenterDelegate`'s completion handlers are plain ObjC
/// blocks that predate `Sendable` auditing, so strict concurrency will not let a
/// `@Sendable` task closure capture them. They are safe to invoke exactly once
/// from any thread, and we always invoke them on the main actor.
private struct UncheckedSendableBox<Value>: @unchecked Sendable {
    let value: Value
}

final class HeraldAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        application.registerForRemoteNotifications()
        return true
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        // Eris does not consume APNs tokens yet. Intentionally unused.
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        // Normal on simulator and on a device with no push entitlement.
    }

    // MARK: UNUserNotificationCenterDelegate
    //
    // The completion-handler form rather than the async form, for the same
    // reason as the legacy build: the compiler-generated thunk for the async
    // form resumes off the main thread and UIKit asserts inside the completion.
    // Hopping to `@MainActor` ourselves keeps that contract.

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        let complete = UncheckedSendableBox(value: completionHandler)
        Task { @MainActor in
            complete.value([.banner, .list, .sound, .badge])
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let complete = UncheckedSendableBox(value: completionHandler)
        Task { @MainActor in
            complete.value()
        }
    }
}

// MARK: - Themed root
//
// The single launch surface: the full Kallisti theme wraps the Eris surface.
// `ErisSurfaceScreen` is the pairing form when the device has no usable pair and
// the paired tab surface once it does, so one root serves first pairing, a
// relaunch straight back into chat, and re-pairing after an expiry.

struct ThemeAwareRootView: View {
    @Environment(\.colorScheme) private var systemScheme
    @Environment(ThemeManager.self) private var themeManager

    var body: some View {
        ErisSurfaceScreen()
            .onChange(of: systemScheme, initial: true) { _, newScheme in
                themeManager.systemScheme = newScheme
            }
            .preferredColorScheme(
                themeManager.colorSchemePreference == .system
                    ? nil
                    : themeManager.colorSchemePreference == .light ? .light : .dark
            )
    }
}

@main
struct HeraldApp: App {
    @UIApplicationDelegateAdaptor(HeraldAppDelegate.self) private var appDelegate

    /// Device permissions (health, motion, location, notifications, microphone,
    /// speech). Independent of the Eris daemon: these are the local TCC grants the
    /// Kallisti identity surfaces, kept here so the Permissions screen has a store
    /// to read and request against without any legacy connector involvement.
    @State private var permissionsStore = PermissionsStore(
        locationService: LiveLocationService(),
        healthService: LiveHealthService(),
        notificationService: LiveNotificationService(),
        mediaService: LiveMediaService(),
        motionService: LiveMotionService()
    )

    var body: some Scene {
        WindowGroup {
            ThemeAwareRootView()
                .environment(ThemeManager.shared)
                .environment(permissionsStore)
        }
    }
}
