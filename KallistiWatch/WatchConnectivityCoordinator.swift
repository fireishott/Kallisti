//
//  WatchConnectivityCoordinator.swift
//  HeraldWatch
//
//  WatchConnectivity session for the Herald watchOS companion. The Watch
//  Hands off these actions to the paired iPhone:
//    - Reply (with text) — triggers one canonical user submission
//    - Stop — cancels the canonical job ID
//    - Read — opens the canonical conversation on iOS
//    - Nudge — explicit retry with the canonical clientMessageId
//
//  Key rules:
//    - Every Watch-originated action carries an `idempotencyId` (UUID), so a
//      duplicate immediateMessage + queued transfer delivery collapses to a
//      single iOS send.
//    - The iOS side returns an `acknowledgement`; we transition the UI from
//      "queued" to "delivered" exactly once per acknowledgement.
//    - When WCSession is not reachable we surface truthful unavailable
//      state instead of silently dropping the action.
//
//  Built 108 — Phase 3W (Watch companion).
//

import Foundation
import WatchConnectivity
import Observation


/// WatchConnectivity session coordinator. Activate on Watch launch. UI
/// observers watch `pendingActions` for queued/unacknowledged state and
/// `lastError` for truthful "phone unreachable" feedback.
@MainActor
@Observable
final class WatchConnectivityCoordinator: NSObject {
    static let shared = WatchConnectivityCoordinator()

    private(set) var isReachable: Bool = false
    private(set) var pendingActions: [UUID: WatchActionEnvelope] = [:]
    private(set) var lastError: String?

    private let session: WCSession? = WCSession.isSupported() ? WCSession.default : nil

    private override init() {
        super.init()
        if let session, session.activationState != .activated {
            session.delegate = self
            session.activate()
        }
    }

    /// Send an action to the iPhone. Uses `sendMessage` when reachable for
    /// immediate delivery; falls back to `transferUserInfo` when not (queued
    /// for next reachable moment). The acknowledgement path is identical in
    /// both cases.
    func send(_ envelope: WatchActionEnvelope) {
        guard let session else {
            lastError = "WatchConnectivity is not supported on this device."
            return
        }
        pendingActions[envelope.idempotencyId] = envelope
        lastError = nil
        if session.isReachable {
            session.sendMessage(envelope.toWCDictionary(), replyHandler: { [weak self] reply in
                Task { @MainActor in self?.handleReply(reply) }
            }, errorHandler: { [weak self] error in
                Task { @MainActor in self?.handleSendError(envelope, error: error) }
            })
        } else {
            session.transferUserInfo(envelope.toWCDictionary())
        }
    }

    /// One acknowledgement, as plain values rather than a dictionary.
    ///
    /// The delegate callbacks run off the main actor and a `[String: Any]` is
    /// not `Sendable`, so the three strings are read where the message arrives
    /// and only values cross to this side.
    /// Store a handoff from the phone and open the conversation.
    func acceptPairing(_ handoff: WatchPairingHandoff) {
        let credentials = handoff.credentials(deviceID: "phone")
        do {
            try WatchPairingStore().save(credentials)
            WatchLaunchState.shared.open(listening: false)
        } catch {
            lastError = "The pairing could not be stored: \(error.localizedDescription)"
        }
    }

    private func handleAcknowledgement(
        idString: String?,
        status: String?,
        errorMessage: String?
    ) {
        guard let idString, let id = UUID(uuidString: idString) else {
            return
        }
        let status = status.flatMap(WatchAckStatus.init(rawValue:)) ?? .accepted
        // Once acknowledged, drop from pending - exactly once per id.
        pendingActions[id] = nil
        if status == .rejected {
            lastError = errorMessage ?? "iOS rejected the action."
        }
    }

    private func handleReply(_ reply: [String: Any]) {
        guard let idString = reply[WatchPayloadKey.idempotencyId.rawValue] as? String,
              let id = UUID(uuidString: idString) else {
            return
        }
        let status = (reply[WatchPayloadKey.acknowledgementStatus.rawValue] as? String)
            .flatMap(WatchAckStatus.init(rawValue:)) ?? .accepted
        // Once acknowledged, drop from pending — exactly once per id.
        if pendingActions[id] != nil {
            pendingActions[id] = nil
        }
        if status == .rejected {
            lastError = reply[WatchPayloadKey.errorMessage.rawValue] as? String ?? "iOS rejected the action."
        }
    }

    private func handleSendError(_ envelope: WatchActionEnvelope, error: Error) {
        // `sendMessage` failed mid-flight. The iOS side may still receive
        // the queued `transferUserInfo` replica. Do NOT remove from pending
        // yet — wait for the iOS ack or a manual retry.
        lastError = error.localizedDescription
        _ = envelope
    }

    /// Remove a pending action when the user explicitly cancels.
    func cancelPending(idempotencyId: UUID) {
        pendingActions[idempotencyId] = nil
    }
}

/// A reply closure outlives the delegate callback that was handed it, and
/// `[String: Any]` is not `Sendable`. The box is asserted rather than proved:
/// the closure is called once, on the main actor, before the box falls out of
/// scope.
private final class WatchReplyBox: @unchecked Sendable {
    private let handler: ([String: Any]) -> Void

    init(_ handler: @escaping ([String: Any]) -> Void) {
        self.handler = handler
    }

    func call(_ payload: [String: Any]) {
        handler(payload)
    }
}

extension WatchConnectivityCoordinator: WCSessionDelegate {
    nonisolated func session(
        _ session: WCSession,
        activationDidCompleteWith activationState: WCSessionActivationState,
        error: Error?
    ) {
        // Read what is needed before the hop: the session is not `Sendable`.
        let reachable = session.isReachable
        let failure = error?.localizedDescription
        Task { @MainActor in
            self.isReachable = reachable
            if let failure {
                self.lastError = failure
            }
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveApplicationContext applicationContext: [String: Any]) {
        // The phone hands over its own pairing: origin, access token, refresh
        // token. An application context is redelivered if the wrist was asleep,
        // so a watch that missed it still pairs on its next run.
        // Decode on this side: the dictionary is not Sendable, the handoff is.
        guard let handoff = WatchPairingHandoff(context: applicationContext) else { return }
        Task { @MainActor in
            self.acceptPairing(handoff)
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any]) {
        // The phone may send an acknowledgement cold (a reply delivered while
        // the wrist was backgrounded). Read the three values it needs here.
        let id = message[WatchPayloadKey.idempotencyId.rawValue] as? String
        let status = message[WatchPayloadKey.acknowledgementStatus.rawValue] as? String
        let failure = message[WatchPayloadKey.errorMessage.rawValue] as? String
        Task { @MainActor in
            self.handleAcknowledgement(idString: id, status: status, errorMessage: failure)
        }
    }

    nonisolated func session(
        _ session: WCSession,
        didReceiveMessage message: [String: Any],
        replyHandler: @escaping ([String: Any]) -> Void
    ) {
        let id = message[WatchPayloadKey.idempotencyId.rawValue] as? String
        let status = message[WatchPayloadKey.acknowledgementStatus.rawValue] as? String
        let failure = message[WatchPayloadKey.errorMessage.rawValue] as? String
        let reply = WatchReplyBox(replyHandler)
        Task { @MainActor in
            self.handleAcknowledgement(idString: id, status: status, errorMessage: failure)
            reply.call([:])
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        // Queued `transferUserInfo` delivery from the phone: the same path.
        let id = userInfo[WatchPayloadKey.idempotencyId.rawValue] as? String
        let status = userInfo[WatchPayloadKey.acknowledgementStatus.rawValue] as? String
        let failure = userInfo[WatchPayloadKey.errorMessage.rawValue] as? String
        Task { @MainActor in
            self.handleAcknowledgement(idString: id, status: status, errorMessage: failure)
        }
    }

    nonisolated func sessionReachabilityDidChange(_ session: WCSession) {
        let reachable = session.isReachable
        Task { @MainActor in
            self.isReachable = reachable
        }
    }
}
