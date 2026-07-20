import Foundation

/// Stable identifiers for notification categories and actions.
enum NotificationCategoryID {
    static let messageReady = "HERALD_MESSAGE_READY"
    static let jobActive = "HERALD_JOB_ACTIVE"
}

enum NotificationActionID {
    static let read = "HERALD_ACTION_READ"
    static let reply = "HERALD_ACTION_REPLY"
    static let stop = "HERALD_ACTION_STOP"
    static let nudge = "HERALD_ACTION_NUDGE"
}

@MainActor
protocol NotificationServiceProtocol {
    var authorizationStatus: PermissionStatus { get }
    var currentPushToken: String? { get }
    var isPushTokenRegistered: Bool { get }
    func requestAuthorization() async -> PermissionStatus
    func refreshAuthorizationStatus() async
    func updatePushToken(_ token: String?) async
    func markPushTokenRegistered(_ registered: Bool) async
    func registerCategories()
}
