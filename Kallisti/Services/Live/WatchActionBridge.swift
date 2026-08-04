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

    init(container: AppContainer) {
        self.container = container
        PhoneWatchConnectivityCoordinator.shared.attach(handler: self)
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
