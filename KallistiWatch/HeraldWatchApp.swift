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
            WatchAppRoot()
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

/// What the app shows: the conversation when something asked for it, the
/// heartbeat otherwise.
struct WatchAppRoot: View {
    @State private var launch = WatchLaunchState.shared
    @State private var runtime: WatchRuntime?

    var body: some View {
        Group {
            if let runtime, launch.openConversation {
                WatchConversationScreen(
                    chooser: runtime.chooser,
                    transport: runtime.streamer,
                    autoListen: launch.startListening
                )
            } else {
                HeartbeatView()
            }
        }
        .task {
            if runtime == nil {
                runtime = WatchRuntime.make()
                // A paired wrist opens on the conversation: the heartbeat is the
                // unpaired state, not a home screen to get past.
                if (try? WatchPairingStore().credentials()) != nil {
                    launch.open(listening: false)
                }
            }
        }
        .onOpenURL { url in
            launch.handle(url)
        }
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
    /// What the system handed this scene. `body` is read after `didReceive`, and
    /// `notificationContent` is not an API of this class - the content arrives
    /// through the only callback the controller has.
    private var content = UNNotificationContent()

    override func didReceive(_ notification: UNNotification) {
        content = notification.request.content
    }

    override var body: WatchNotificationRoot {
        let notification = content
        let payload = WatchNotificationPayload.decode(from: notification.userInfo)
        return WatchNotificationRoot(
            payload: payload,
            fallbackTitle: notification.title.isEmpty ? "Kallisti" : notification.title,
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
    // `SyntheticNotification` is private to this file, and this initializer is
    // its only user; an internal one could not name the type.
    fileprivate init(payload: WatchNotificationPayload?, notification: SyntheticNotification) {
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
            Text("Kallisti")
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
