//
//  WatchActionBridge.swift
//  Herald
//
//  Bridges the iOS-side WatchConnectivity coordinator into the canonical
//  AppContainer notification handler. The bridge is constructed once at
//  app launch and wires the Watch actions into the same identity-anchored
//  pipeline that lock-screen notification actions use.
//
//  Built 108 — Phase 3W (Watch companion).
//

import Foundation

/// Bridges `PhoneWatchConnectivityCoordinator` to AppContainer. Implements
/// `WatchActionHandler` so the coordinator can dispatch Watch actions into
/// the canonical notification-action pipeline.
@MainActor
final class WatchActionBridge: WatchActionHandler {
    private let container: AppContainer

    /// The one bridge this process keeps alive.
    ///
    /// The coordinator holds its handler weakly, so a bridge nobody retains is
    /// deallocated the moment launch finishes and every wrist action would then
    /// answer `unavailable` for the life of the install.
    private static var retained: WatchActionBridge?

    /// Build the bridge once, at launch, and keep it.
    static func install(container: AppContainer) {
        guard retained == nil else { return }
        retained = WatchActionBridge(container: container)
    }

    init(container: AppContainer) {
        self.container = container
        PhoneWatchConnectivityCoordinator.shared.attach(handler: self)
        // The wrist pairs by being handed the phone's own pairing: the same
        // daemon, the same origin, the same tokens. A wrist that is asleep gets
        // it the next time it runs, because an application context persists.
        PhoneWatchConnectivityCoordinator.shared.attach { [container] in
            guard let base = container.settingsStore.settings
                .relayConfiguration.activeBaseURLString, !base.isEmpty,
                let origin = URL(string: base)
            else { return nil }
            // The wrist gets its own device and its own token pair. Copying the
            // phone's pair made both clients refresh one family of tokens, so
            // whichever refreshed first retired the other and the phone asked to
            // be paired again. `POST /v1/watch/pair` is the route that exists for
            // exactly this, and it also answers the origin the wrist can dial.
            let access = await container.sessionStore.currentAccessToken() ?? ""
            guard !access.isEmpty else { return nil }
            var request = URLRequest(url: origin.appending(path: "/v1/watch/pair"))
            request.httpMethod = "POST"
            request.timeoutInterval = 20
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue("Bearer \(access)", forHTTPHeaderField: "Authorization")
            request.httpBody = try? JSONSerialization.data(withJSONObject: ["deviceName": "watch-wrist"])
            guard let (data, response) = try? await URLSession.shared.data(for: request),
                  let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let watchAccess = object["access_token"] as? String,
                  let watchRefresh = object["refresh_token"] as? String
            else { return nil }
            let lan = (object["lan"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            return WatchPairingHandoff(
                baseURL: lan ?? base,
                accessToken: watchAccess,
                refreshToken: watchRefresh
            )
        }
    }

    func handleReply(
        conversationId: UUID?,
        canonicalMessageId: String?,
        jobId: UUID?,
        replyText: String?
    ) async {
        container.handleWatchAction(
            action: NotificationActionID.reply.rawValue,
            conversationID: conversationId,
            messageID: canonicalMessageId,
            jobID: jobId?.uuidString,
            replyText: replyText
        )
    }

    func handleStop(jobId: UUID?) async {
        container.handleWatchAction(
            action: NotificationActionID.stop.rawValue,
            conversationID: nil,
            messageID: nil,
            jobID: jobId?.uuidString,
            replyText: nil
        )
    }

    func handleRead(conversationId: UUID?) async {
        container.handleWatchAction(
            action: NotificationActionID.read.rawValue,
            conversationID: conversationId,
            messageID: nil,
            jobID: nil,
            replyText: nil
        )
    }

    func handleNudge(conversationId: UUID?, canonicalMessageId: String?) async {
        container.handleWatchAction(
            action: NotificationActionID.nudge.rawValue,
            conversationID: conversationId,
            messageID: canonicalMessageId,
            jobID: nil,
            replyText: nil
        )
    }

    func handleRemindLater(conversationId: UUID?) async {
        container.handleWatchAction(
            action: NotificationActionID.remindLater.rawValue,
            conversationID: conversationId,
            messageID: nil,
            jobID: nil,
            replyText: nil
        )
    }
}
