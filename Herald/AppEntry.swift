import SwiftUI
import UIKit
import UserNotifications
import BackgroundTasks

/// Carries a UIKit completion block across a concurrency hop.
///
/// `UNUserNotificationCenterDelegate`'s completion handlers are plain ObjC
/// blocks that predate `Sendable` auditing, so strict concurrency will not let
/// a `@Sendable` task closure capture them. They are safe to invoke exactly
/// once from any thread, and we always invoke them on the main actor — which
/// is what UIKit requires (see `HeraldAppDelegate`'s delegate methods).
private struct UncheckedSendableBox<Value>: @unchecked Sendable {
    let value: Value
}

final class HeraldAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        // Do not tear down existing Live Activities on launch. ActivityKit owns
        // their background lifetime, and the active turn needs to remain visible
        // after the app is minimized or relaunched. LiveActivityService adopts
        // an existing activity when the chat resumes and ends it on terminal
        // turn states.

        // Register background refresh task
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: "net.fihonline.kallisti.refresh",
            using: nil
        ) { task in
            Task { @MainActor in
                await AppContainer.sharedDefault().handleBackgroundRefresh(task as! BGAppRefreshTask)
            }
        }
        scheduleBackgroundRefresh()

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

    /// Show banner + sound while app is in the foreground.
    ///
    /// Implements nav-aware suppression (hermes-mobile pattern): when the user
    /// is actively viewing the same conversation that the notification is for,
    /// suppress the banner to avoid self-notifications. All other conversations
    /// still deliver banners normally.
    ///
    /// Implemented in its completion-handler form rather than the `async` form.
    /// The compiler-generated `@objc` thunk for a `nonisolated async` delegate
    /// method resumes on the cooperative pool, so it calls UIKit's completion
    /// block off the main thread — and UIKit asserts on the main thread inside
    /// it (`_performBlockAfterCATransactionCommitSynchronizes:`), aborting the
    /// process. Hopping to `@MainActor` ourselves and completing there is what
    /// keeps that contract. `UNNotification` is not Sendable, so every value we
    /// need is read here, on the delivering thread, before the hop.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        let notificationConversationId =
            notification.request.content.userInfo["conversationId"] as? String
        let complete = UncheckedSendableBox(value: completionHandler)

        Task { @MainActor in
            let container = AppContainer.sharedDefault()
            guard container.settingsStore.settings.notificationsEnabled else {
                complete.value([])
                return
            }

            // Nav-aware suppression: if the user is viewing the same conversation
            // the notification is for, suppress to avoid self-notification.
            if let notificationConversationId,
               let currentId = container.chatStore.conversation?.id,
               notificationConversationId == currentId.uuidString.lowercased() {
                complete.value([])
                return
            }

            complete.value([.banner, .list, .sound, .badge])
        }
    }

    // Handle tap on notification — deep-link into the conversation.
    //
    // Completion-handler form for the same reason as `willPresent` above: the
    // thunk for the `async` form completed on a background thread, and UIKit's
    // completion runs `_updateSnapshotAndStateRestoration…`, which asserts it
    // is on the main thread. That assertion was the SIGABRT on every
    // notification tap. `UNNotificationResponse` is not Sendable, so only
    // primitives cross to the main actor.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        let conversationIDString = info["conversationId"] as? String
        let conversationID = conversationIDString.flatMap { UUID(uuidString: $0) }
        let messageID = info["messageId"] as? String
        let jobID = info["jobId"] as? String
        let action = response.actionIdentifier

        // Extract reply text for Reply action
        var replyText: String?
        if action == NotificationActionID.reply.rawValue,
           let textResponse = response as? UNTextInputNotificationResponse {
            replyText = textResponse.userText
        }

        let complete = UncheckedSendableBox(value: completionHandler)

        Task { @MainActor in
            let container = AppContainer.sharedDefault()
            container.handleNotificationRoute(
                conversationID: conversationID,
                messageID: messageID,
                jobID: jobID,
                action: action,
                replyText: replyText
            )
            complete.value()
        }
    }

    private func scheduleBackgroundRefresh() {
        let request = BGAppRefreshTaskRequest(identifier: "net.fihonline.kallisti.refresh")
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
        try? BGTaskScheduler.shared.submit(request)
    }
}

struct ThemeAwareRootView: View {
    @Environment(\.colorScheme) private var systemScheme
    @Environment(ThemeManager.self) private var themeManager
    let container: AppContainer

    var body: some View {
        AppRootView()
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
    @Environment(\.scenePhase) private var scenePhase
    @UIApplicationDelegateAdaptor(HeraldAppDelegate.self) private var appDelegate
    @State private var container = AppContainer.sharedDefault()

    /// Build 26 (keep-awake): identifier of the background task that keeps
    /// the app process running while backgrounded so the gateway WebSocket
    /// survives short background periods. Invalid means no task is active.
    @State private var gatewayKeepAwakeTaskID: UIBackgroundTaskIdentifier = .invalid

    private func beginGatewayKeepAwake() {
        guard gatewayKeepAwakeTaskID == .invalid else { return }
        gatewayKeepAwakeTaskID = UIApplication.shared.beginBackgroundTask(withName: "kallisti.gateway.keepalive") {
            // Expired - iOS will suspend us. Persist in-flight state so the
            // next launch can show a "resuming" indicator instead of a blank screen.
            Task { @MainActor in
                AppContainer.sharedDefault().chatStore.persistInFlightCheckpointIfActive()
            }
            endGatewayKeepAwake()
        }
    }

    private func endGatewayKeepAwake() {
        guard gatewayKeepAwakeTaskID != .invalid else { return }
        UIApplication.shared.endBackgroundTask(gatewayKeepAwakeTaskID)
        gatewayKeepAwakeTaskID = .invalid
    }

    var body: some Scene {
        WindowGroup {
            ThemeAwareRootView(container: container)
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
                .environment(container.dashboardLogService)
                .environment(container.gatewayControl)
                .task { await container.initialize() }
                .onChange(of: scenePhase) { _, newPhase in
                    if newPhase == .active {
                        // Build 52 (sleep recovery): tell ChatStore the app is
                        // foregrounded again so the streaming watchdog stops
                        // excluding suspended wall time and the resume path
                        // (session.resume) can reattach a parked session.
                        container.chatStore.markStreamForegrounded()
                        Task { await container.handleAppDidBecomeActive() }
                        // Reset badge count when the user returns to the app
                        UNUserNotificationCenter.current().setBadgeCount(0) { _ in }
                        // End the keep-awake window started when we backgrounded
                        endGatewayKeepAwake()
                    } else if newPhase == .background {
                        // Build 52 (sleep recovery): a stream in flight when we
                        // background is frozen by iOS. Exclude this wall time
                        // from the watchdog and remember that a resume may be
                        // needed on foreground.
                        container.chatStore.markStreamBackgrounded()
                        Task {
                            await container.reportAppStateIfNeeded("background")
                            // Stop real-time host status stream when backgrounded
                            await container.hostStatusStream.stop()
                        }
                        // Build 26 (keep-awake fix): iOS suspends the app
                        // within seconds of backgrounding, which silently
                        // kills the gateway WebSocket (receive() never
                        // surfaces the error). Holding a background task
                        // keeps the process running long enough for the
                        // transport's keepalive pings to keep the socket
                        // alive through short background periods, so the
                        // next message doesn't wait behind a phantom-socket
                        // reconnect. The task ends on foreground or when iOS
                        // expires it (~30s), whichever comes first.
                        beginGatewayKeepAwake()
                    }
                    // Note: voice sessions are NOT ended on background.
                    // The "audio" background mode keeps the voice session alive
                    // so the user can continue talking while the app is
                    // backgrounded. The session ends only when the user
                    // explicitly closes the voice overlay.
                }
                .onContinueUserActivity(QuickNoteConstants.activityType) { activity in
                    handleQuickNoteActivity(activity)
                }
                .onOpenURL { url in
                    handleDeeplink(url)
                }
        }
    }

    private func handleQuickNoteActivity(_ activity: NSUserActivity) {
        guard activity.activityType == QuickNoteConstants.activityType,
              let identifier = activity.targetContentIdentifier,
              let noteId = QuickNoteConstants.noteId(from: identifier) else { return }
        Task { @MainActor in
            await container.notesStore.loadNotes()
            container.notesStore.selectNote(noteId)
            container.router.switchToTab(.notes)
        }
    }

    private func handleDeeplink(_ url: URL) {
        guard url.scheme == "kallisti" else { return }
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
        case "share":
            if let params = ShareURLParser.parse(url) {
                Task { @MainActor in
                    let note = await container.notesStore.createNoteFromSharedText(
                        params.text,
                        title: params.title
                    )
                    if let noteId = note?.id {
                        container.notesStore.selectNote(noteId)
                    }
                    container.router.switchToTab(.notes)
                }
            }
        case "voice":
            container.router.isVoiceOverlayPresented = true
        default:
            break
        }
    }
}
