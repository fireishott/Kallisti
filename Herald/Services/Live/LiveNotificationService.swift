import Foundation
import UserNotifications

@MainActor
@Observable
final class LiveNotificationService: NotificationServiceProtocol {
    private(set) var authorizationStatus: PermissionStatus = .notDetermined
    private(set) var currentPushToken: String?
    private(set) var isPushTokenRegistered: Bool = false

    private let center = UNUserNotificationCenter.current()

    init() {
        Task { await refreshAuthorizationStatus() }
    }

    func requestAuthorization() async -> PermissionStatus {
        do {
            let granted = try await center.requestAuthorization(options: [.alert, .badge, .sound])
            authorizationStatus = granted ? .authorized : .denied
        } catch {
            authorizationStatus = .denied
        }
        return authorizationStatus
    }

    func updatePushToken(_ token: String?) async {
        currentPushToken = token
    }

    func markPushTokenRegistered(_ registered: Bool) async {
        isPushTokenRegistered = registered
    }

    func refreshAuthorizationStatus() async {
        let settings = await center.notificationSettings()
        authorizationStatus = mapStatus(settings.authorizationStatus)
    }

    func registerCategories() {
        // HERALD_MESSAGE_READY: completed chat reply
        let readAction = UNNotificationAction(
            identifier: NotificationActionID.read,
            title: "Read",
            options: .foreground
        )
        let replyAction = UNTextInputNotificationAction(
            identifier: NotificationActionID.reply,
            title: "Reply",
            options: [],
            textInputButtonTitle: "Send",
            textInputPlaceholder: "Type your reply..."
        )
        let nudgeAction = UNNotificationAction(
            identifier: NotificationActionID.nudge,
            title: "Nudge",
            options: []
        )
        let messageReadyCategory = UNNotificationCategory(
            identifier: NotificationCategoryID.messageReady,
            actions: [readAction, replyAction, nudgeAction],
            intentIdentifiers: [],
            options: []
        )

        // HERALD_JOB_ACTIVE: job is still running
        let stopAction = UNNotificationAction(
            identifier: NotificationActionID.stop,
            title: "Stop",
            options: [.destructive]
        )
        let jobActiveCategory = UNNotificationCategory(
            identifier: NotificationCategoryID.jobActive,
            actions: [readAction, stopAction, nudgeAction],
            intentIdentifiers: [],
            options: []
        )

        center.setNotificationCategories([messageReadyCategory, jobActiveCategory])
    }

    private func mapStatus(_ status: UNAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .notDetermined: .notDetermined
        case .denied: .denied
        case .authorized: .authorized
        case .provisional: .limited
        case .ephemeral: .limited
        @unknown default: .notDetermined
        }
    }
}
