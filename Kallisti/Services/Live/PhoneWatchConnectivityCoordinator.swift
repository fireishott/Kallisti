//
//  PhoneWatchConnectivityCoordinator.swift
//  Herald
//
//  iOS-side WatchConnectivity session. The paired Watch sends action
//  envelopes (Reply / Stop / Read / Nudge / Remind-Later) through this
//  coordinator. The iOS side routes them through the same canonical
//  identity paths that the iOS notification handler uses, sharing the
//  phase-3 reducer and conversationId-anchored user submission.
//
//  Idempotency: every Watch-originated action carries an `idempotencyId`.
//  This coordinator de-duplicates (collapsed) repeated envelopes — when
//  both immediateMessage and transferUserInfo race, we surface a single
//  iOS send and the second arrival is reported as `.duplicate`.
//
//  Built 108 — Phase 3W (Watch companion).
//

import Foundation
import WatchConnectivity
import Observation

/// iOS-side handler that knows how to deliver a Watch action into the same
/// canonical pipeline as a lock-screen notification action. The AppContainer
/// owns the concrete implementation; this protocol keeps the coordinator
/// testable.
@MainActor
protocol WatchActionHandler: AnyObject {
    func handleReply(
        conversationId: UUID?,
        canonicalMessageId: String?,
        jobId: UUID?,
        replyText: String?
    ) async
    func handleStop(jobId: UUID?) async
    func handleRead(conversationId: UUID?) async
    func handleNudge(conversationId: UUID?, canonicalMessageId: String?) async
    func handleRemindLater(conversationId: UUID?) async
}

@MainActor
@Observable
final class PhoneWatchConnectivityCoordinator: NSObject {
    static let shared = PhoneWatchConnectivityCoordinator()

    private weak var handler: WatchActionHandler?

    /// Recent Watch-originated idempotency IDs, used to collapse duplicate
    /// immediateMessage + transferUserInfo deliveries. The size is bounded
    /// by `maxIdempotencyWindow`.
    private var recentIdempotencyIds: [UUID: Date] = [:]
    private let maxIdempotencyWindow: TimeInterval = 60

    private(set) var isReachable: Bool = false
    private(set) var lastDuplicateCollapsed: Date?

    private let session: WCSession? = WCSession.isSupported() ? WCSession.default : nil

    private override init() {
        super.init()
        if let session {
            session.delegate = self
            session.activate()
        }
    }

    /// Wire the AppContainer handler. The coordinator is intentionally
    /// decoupled from the rest of the app so tests can substitute a mock.
    func attach(handler: WatchActionHandler) {
        self.handler = handler
    }

    /// Drop entries older than the idempotency window. Run on every inbound
    /// action so the dictionary stays small.
    private func pruneIdempotencyCache(now: Date = Date()) {
        let cutoff = now.addingTimeInterval(-maxIdempotencyWindow)
        recentIdempotencyIds = recentIdempotencyIds.filter { $0.value >= cutoff }
    }

    /// Returns `.duplicate` if the idempotencyId was already seen within
    /// the window. Otherwise records it and returns `.accepted`.
    private func recordOrCollapse(idempotencyId: UUID) -> WatchAckStatus {
        pruneIdempotencyCache()
        if recentIdempotencyIds[idempotencyId] != nil {
            lastDuplicateCollapsed = Date()
            return .duplicate
        }
        recentIdempotencyIds[idempotencyId] = Date()
        return .accepted
    }

    private func reply(for envelope: WatchActionEnvelope, status: WatchAckStatus) -> [String: Any] {
        [
            WatchPayloadKey.idempotencyId.rawValue: envelope.idempotencyId.uuidString,
            WatchPayloadKey.acknowledgementStatus.rawValue: status.rawValue
        ]
    }

    private func dispatch(_ envelope: WatchActionEnvelope) async -> WatchAckStatus {
        guard let handler else { return .unavailable }
        let status = recordOrCollapse(idempotencyId: envelope.idempotencyId)
        if status == .duplicate {
            return .duplicate
        }
        switch envelope.action {
        case .reply:
            await handler.handleReply(
                conversationId: envelope.conversationId,
                canonicalMessageId: envelope.canonicalMessageId,
                jobId: envelope.jobId,
                replyText: envelope.replyText
            )
        case .stop:
            await handler.handleStop(jobId: envelope.jobId)
        case .read:
            await handler.handleRead(conversationId: envelope.conversationId)
        case .nudge:
            await handler.handleNudge(
                conversationId: envelope.conversationId,
                canonicalMessageId: envelope.canonicalMessageId
            )
        }
        return .accepted
    }
}

extension PhoneWatchConnectivityCoordinator: WCSessionDelegate {
    nonisolated func session(_ session: WCSession, activationDidCompleteWith activationState: WCSessionActivationState, error: Error?) {
        Task { @MainActor in
            self.isReachable = session.isReachable
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any]) {
        // Fire-and-forget immediate delivery; the canonical reply path on
        // Watch only needs the acknowledgement.
        Task { @MainActor in
            await self.handleRawMessage(message)
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any], replyHandler: @escaping ([String: Any]) -> Void) {
        Task { @MainActor in
            let status = await self.handleRawMessage(message)
            if let envelope = WatchActionEnvelope.fromWCDictionary(message) {
                replyHandler(self.reply(for: envelope, status: status))
            } else {
                replyHandler([WatchPayloadKey.acknowledgementStatus.rawValue: WatchAckStatus.rejected.rawValue])
            }
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        Task { @MainActor in
            await self.handleRawMessage(userInfo)
        }
    }

    nonisolated func sessionReachabilityDidChange(_ session: WCSession) {
        Task { @MainActor in self.isReachable = session.isReachable }
    }

    @MainActor
    private func handleRawMessage(_ message: [String: Any]) async -> WatchAckStatus {
        guard let envelope = WatchActionEnvelope.fromWCDictionary(message) else {
            return .rejected
        }
        return await dispatch(envelope)
    }
}
