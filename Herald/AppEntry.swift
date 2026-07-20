import SwiftUI
import UIKit
import UserNotifications

final class HeraldAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        // If the app was previously killed while a Live Activity was active,
        // the OS can still show that stale activity. Clear any orphaned Herald
        // activities immediately on launch; real active sessions will recreate
        // or adopt an activity once state is restored.
        LiveActivityService.endAllActivities()

        // Register for remote (silent push) notifications
        application.registerForRemoteNotifications()

        // Set up notification center delegate for foreground banners and tap handling
        UNUserNotificationCenter.current().delegate = self

        Task { @MainActor in
            await AppContainer.sharedDefault().handleSystemLaunch()
        }
        return true
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        let token = deviceToken.map { String(format: "%02.2hhx", $0) }.joined()
        Task { @MainActor in
            await AppContainer.sharedDefault().persistAndRegisterAPNsToken(token)
        }
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        // Push token registration failed — this is normal on simulators
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        // Handle silent push without marking the app foreground.
        Task { @MainActor in
            let container = AppContainer.sharedDefault()
            await container.handleRemoteNotificationWake()
            completionHandler(.newData)
        }
    }

    // MARK: - UNUserNotificationCenterDelegate

    // Show banner + sound while app is in the foreground.
    // nonisolated because UNNotification is not Sendable.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        let enabled = await MainActor.run {
            AppContainer.sharedDefault().settingsStore.settings.notificationsEnabled
        }
        guard enabled else { return [] }
        return [.banner, .list, .sound, .badge]
    }

    // Handle tap on notification — deep-link into the conversation.
    // nonisolated because UNNotificationResponse is not Sendable.
    // Extracts primitive strings only, then delegates to AppContainer.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let info = response.notification.request.content.userInfo
        let conversationIDString = info["conversationId"] as? String
        let conversationID = conversationIDString.flatMap { UUID(uuidString: $0) }
        let messageID = info["messageId"] as? String
        let jobID = info["jobId"] as? String
        let action = response.actionIdentifier

        // Extract reply text for Reply action
        var replyText: String?
        if action == NotificationActionID.reply,
           let textResponse = response as? UNTextInputNotificationResponse {
            replyText = textResponse.userText
        }

        await MainActor.run {
            let container = AppContainer.sharedDefault()
            container.handleNotificationRoute(
                conversationID: conversationID,
                messageID: messageID,
                jobID: jobID,
                action: action,
                replyText: replyText
            )
        }
    }
}

@main
struct HeraldApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @UIApplicationDelegateAdaptor(HeraldAppDelegate.self) private var appDelegate
    @State private var container = AppContainer.sharedDefault()

    var body: some Scene {
        WindowGroup {
            AppRootView()
                .environment(container)
                .environment(container.router)
                .environment(container.themeManager)
                .environment(container.sessionStore)
                .environment(container.pairingStore)
                .environment(container.hostStore)
                .environment(container.chatStore)
                .environment(container.inboxStore)
                .environment(container.permissionsStore)
                .environment(container.settingsStore)
                .environment(container.talkStore)
                .environment(container.sessionListStore)
                .environment(container.modelStore)
                .environment(container.profileStore)
                .environment(container.skillsStore)
                .environment(container.cronStore)
                .environment(container.canvasStore)
                .environment(container.notesStore)
                .environment(container.attachmentService)
                .task { await container.initialize() }
                .onChange(of: scenePhase) { _, newPhase in
                    if newPhase == .active {
                        Task { await container.handleAppDidBecomeActive() }
                    } else if newPhase == .background {
                        Task { await container.reportAppStateIfNeeded("background") }
                    }
                    // Note: voice sessions are NOT ended on background.
                    // The "audio" background mode keeps WebRTC alive so
                    // the user can continue talking while the app is
                    // backgrounded. The session ends only when the user
                    // explicitly closes the voice overlay.
                }
                .onOpenURL { url in
                    handleDeeplink(url)
                }
        }
    }

    private func handleDeeplink(_ url: URL) {
        guard url.scheme == "herald" else { return }
        switch url.host {
        case "chat":
            container.router.activeSheet = nil
            container.router.popToRoot()
            container.router.switchToTab(.chat)
        case "health":
            container.router.activeSheet = nil
            container.router.popToRoot()
            container.router.switchToTab(.chat)
            container.router.navigate(to: .permissions)
        case "voice":
            container.router.isVoiceOverlayPresented = true
        default:
            break
        }
    }
}
