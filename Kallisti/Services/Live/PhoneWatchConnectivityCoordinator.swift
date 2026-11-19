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

    /// Builds the pairing to hand the wrist, or nil when there is nothing to give.
    private var pairingProvider: (@MainActor () async -> WatchPairingHandoff?)?

    /// Recent Watch-originated idempotency IDs, used to collapse duplicate
    /// immediateMessage + transferUserInfo deliveries. The size is bounded
    /// by `maxIdempotencyWindow`.
    private var recentIdempotencyIds: [UUID: Date] = [:]
    private let maxIdempotencyWindow: TimeInterval = 60

    private(set) var isReachable: Bool = false
    private(set) var lastDuplicateCollapsed: Date?

    private let session: WCSession? = WCSession.isSupported() ? WCSession.default : nil

    /// Hand the wrist the daemon pairing, now and whenever it becomes reachable.
    func attach(pairing: @escaping @MainActor () async -> WatchPairingHandoff?) {
        pairingProvider = pairing
        Task { await pushPairing() }
    }

    /// Push the pairing if there is one and the session is up. An application
    /// context survives a sleeping wrist: WatchConnectivity delivers it again
    /// when the watch next runs.
    func pushPairing() async {
        guard let session, session.activationState == .activated,
              let handoff = await pairingProvider?()
        else { return }
        do {
            try session.updateApplicationContext(handoff.context)
        } catch {
            // Not fatal: a wrist that cannot take it now can take it later.
            lastDuplicateCollapsed = nil
        }
    }

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

/// A reply closure outlives the delegate callback that was handed it, and
/// `[String: Any]` is not `Sendable`. The box is the one place that is asserted
/// rather than proved: the closure is called exactly once, from the main actor,
/// before the box itself falls out of scope.
private final class ReplyHandlerBox: @unchecked Sendable {
    private let handler: ([String: Any]) -> Void

    init(_ handler: @escaping ([String: Any]) -> Void) {
        self.handler = handler
    }

    func call(_ payload: [String: Any]) {
        handler(payload)
    }
}

extension PhoneWatchConnectivityCoordinator: WCSessionDelegate {
    nonisolated func session(
        _ session: WCSession,
        activationDidCompleteWith activationState: WCSessionActivationState,
        error: Error?
    ) {
        // Read what is needed before the hop: the session itself is not
        // `Sendable`, and nothing after this line needs it.
        let reachable = session.isReachable
        Task { @MainActor in
            self.isReachable = reachable
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any]) {
        // Decode first, for the same reason: the envelope is a value, the
        // dictionary it came from is not.
        guard let envelope = WatchActionEnvelope.fromWCDictionary(message) else {
            return
        }
        Task { @MainActor in
            _ = await self.dispatch(envelope)
        }
    }

    nonisolated func session(
        _ session: WCSession,
        didReceiveMessage message: [String: Any],
        replyHandler: @escaping ([String: Any]) -> Void
    ) {
        let envelope = WatchActionEnvelope.fromWCDictionary(message)
        let reply = ReplyHandlerBox(replyHandler)
        Task { @MainActor in
            guard let envelope else {
                reply.call([
                    WatchPayloadKey.acknowledgementStatus.rawValue:
                        WatchAckStatus.rejected.rawValue
                ])
                return
            }
            let status = await self.dispatch(envelope)
            reply.call(self.reply(for: envelope, status: status))
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        guard let envelope = WatchActionEnvelope.fromWCDictionary(userInfo) else {
            return
        }
        Task { @MainActor in
            _ = await self.dispatch(envelope)
        }
    }

    nonisolated func sessionReachabilityDidChange(_ session: WCSession) {
        let reachable = session.isReachable
        Task { @MainActor in
            self.isReachable = reachable
            if reachable {
                // A wrist that just woke is the moment it can take a pairing.
                await self.pushPairing()
            }
        }
    }

    // The two requirements that exist on iOS only. A watch has no second watch to
    // deactivate for, so they are absent there rather than empty.
    #if os(iOS)
    nonisolated func sessionDidBecomeInactive(_ session: WCSession) {
        // Another watch took over. Stay activated: deactivating here would drop
        // the session for the watch that is still wearing it.
    }

    nonisolated func sessionDidDeactivate(_ session: WCSession) {
        // Reactivate so the swapped-out watch can pair again later.
        session.activate()
    }
    #endif
}
