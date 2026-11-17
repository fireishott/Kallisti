import Foundation

// Shared category/action identifiers now live in
// `Herald/Services/Protocols/NotificationCategories.swift`. They are mirrored
// to the HeraldNotificationService and HeraldWatch targets via project.yml.

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
