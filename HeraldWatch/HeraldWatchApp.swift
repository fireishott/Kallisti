//
//  HeraldWatchApp.swift
//  HeraldWatch
//
//  Modern single-target SwiftUI watchOS app. The notification scene is
//  wired through `NotificationScene(_:)` so that the long-look presentation
//  is handled by `HeraldNotificationScene` rather than the legacy
//  WatchKit controller path.
//
//  Built 108 — Phase 3W (Watch companion).
//

import SwiftUI
import WatchKit
import UserNotifications

@main
struct HeraldWatchApp: App {
    @WKApplicationDelegateAdaptor private var appDelegate: HeraldWatchAppDelegate

    var body: some Scene {
        WindowGroup {
            HeartbeatView()
        }
        WKNotificationScene(
            controller: HeraldNotificationSceneController.self,
            category: NotificationCategoryID.messageReady.rawValue
        )
        WKNotificationScene(
            controller: HeraldNotificationSceneController.self,
            category: NotificationCategoryID.jobActive.rawValue
        )
        WKNotificationScene(
            controller: HeraldNotificationSceneController.self,
            category: NotificationCategoryID.sessionReminder.rawValue
        )
    }
}

/// AppDelegate activates the WatchConnectivity session early. We don't
/// bring up the WatchConnectivity in `init` because the SwiftUI lifecycle
/// fires on the main actor and `WCSession.activate()` wants the delegate
/// already wired.
final class HeraldWatchAppDelegate: NSObject, WKApplicationDelegate {
    func applicationDidFinishLaunching() {
        _ = WatchConnectivityCoordinator.shared
    }
}

/// WatchKit-side controller backing the `WKNotificationScene`. The system
/// instantiates this per-incoming notification and passes the `UNNotification`
/// to it. We hand off to the SwiftUI presentation path.
final class HeraldNotificationSceneController: WKUserNotificationHostingController<WatchNotificationRoot> {
    override var body: WatchNotificationRoot {
        guard let notification = notificationContent else {
            return WatchNotificationRoot(payload: nil, fallbackTitle: "Herald", fallbackBody: "")
        }
        let payload = WatchNotificationPayload.decode(from: notification.userInfo)
        return WatchNotificationRoot(
            payload: payload,
            fallbackTitle: notification.title ?? "Herald",
            fallbackBody: WatchNotificationPayload.sanitize(notification.body)
        )
    }
}

/// SwiftUI root for the notification presentation. Standalone (not a full
/// view tree) so WKUserNotificationHostingController can host it.
struct WatchNotificationRoot: View {
    let payload: WatchNotificationPayload?
    let fallbackTitle: String
    let fallbackBody: String

    var body: some View {
        let viewModel = NotificationViewModel(
            payload: payload,
            notification: SyntheticNotification(title: fallbackTitle, body: fallbackBody)
        )
        HeraldNotificationView(viewModel: viewModel, coordinator: WatchConnectivityCoordinator.shared)
    }
}

/// Synthesizes a UNNotificationContent-shaped value for the standalone
/// SwiftUI presentation path. The system supplies the real one to the
/// controller-backed scene.
private struct SyntheticNotification {
    let title: String
    let body: String
}

extension SyntheticNotification {
    var userInfo: [AnyHashable: Any] {
        [:]
    }
}

extension NotificationViewModel {
    init(payload: WatchNotificationPayload?, notification: SyntheticNotification) {
        self.payload = payload
        self.title = payload?.title ?? notification.title
        self.body = payload?.sanitizedPreview ?? notification.body
        self.category = payload?.category
        self.isValid = payload != nil
    }
}

/// Heartbeat view shown when the Watch app is foregrounded without a
/// notification. Confirms the app is alive and the WatchConnectivity
/// session is reachable.
struct HeartbeatView: View {
    @State private var coordinator = WatchConnectivityCoordinator.shared

    var body: some View {
        VStack(spacing: 6) {
            Image(systemName: "shield.lefthalf.filled")
                .imageScale(.large)
                .foregroundStyle(.tint)
            Text("Herald")
                .font(.headline)
            Text(coordinator.isReachable ? "Phone reachable" : "Phone unreachable")
                .font(.footnote)
                .foregroundStyle(.secondary)
            if let lastError = coordinator.lastError {
                Text(lastError)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
        }
        .padding()
    }
}
