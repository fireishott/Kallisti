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

/// Wire contract for a Watch-originated action that the iOS side will execute.
enum WatchAction: String, Sendable {
    case reply
    case stop
    case read
    case nudge
}

/// Encode/decode WatchConnectivity payload keys. Keep in sync with the iOS
/// side coordinator and the shared `NotificationPayloadKey` set.
enum WatchPayloadKey: String, Sendable {
    case action = "action"
    case conversationId = "conversationId"
    case canonicalMessageId = "canonicalMessageId"
    case jobId = "jobId"
    case clientMessageId = "clientMessageId"
    case idempotencyId = "idempotencyId"
    case replyText = "replyText"
    case contractVersion = "contractVersion"
    case acknowledgementStatus = "ackStatus"
    case errorMessage = "error"
}

enum WatchAckStatus: String, Sendable {
    case accepted
    case duplicate
    case rejected
    case unavailable
}

/// One Watch-originated message envelope.
struct WatchActionEnvelope: Sendable, Equatable {
    let action: WatchAction
    let idempotencyId: UUID
    let conversationId: UUID?
    let canonicalMessageId: String?
    let jobId: UUID?
    let clientMessageId: UUID?
    let replyText: String?
    let sentAt: Date

    func toWCDictionary() -> [String: Any] {
        var dict: [String: Any] = [
            WatchPayloadKey.action.rawValue: action.rawValue,
            WatchPayloadKey.idempotencyId.rawValue: idempotencyId.uuidString,
            WatchPayloadKey.contractVersion.rawValue: NotificationContractVersion.current,
            "ts": sentAt.timeIntervalSince1970
        ]
        if let v = conversationId { dict[WatchPayloadKey.conversationId.rawValue] = v.uuidString }
        if let v = canonicalMessageId { dict[WatchPayloadKey.canonicalMessageId.rawValue] = v }
        if let v = jobId { dict[WatchPayloadKey.jobId.rawValue] = v.uuidString }
        if let v = clientMessageId { dict[WatchPayloadKey.clientMessageId.rawValue] = v.uuidString }
        if let v = replyText { dict[WatchPayloadKey.replyText.rawValue] = v }
        return dict
    }

    static func fromWCDictionary(_ dict: [String: Any]) -> WatchActionEnvelope? {
        guard let actionString = dict[WatchPayloadKey.action.rawValue] as? String,
              let action = WatchAction(rawValue: actionString),
              let idString = dict[WatchPayloadKey.idempotencyId.rawValue] as? String,
              let idempotencyId = UUID(uuidString: idString) else {
            return nil
        }
        let conversationId = (dict[WatchPayloadKey.conversationId.rawValue] as? String)
            .flatMap { UUID(uuidString: $0) }
        let canonicalMessageId = dict[WatchPayloadKey.canonicalMessageId.rawValue] as? String
        let jobId = (dict[WatchPayloadKey.jobId.rawValue] as? String)
            .flatMap { UUID(uuidString: $0) }
        let clientMessageId = (dict[WatchPayloadKey.clientMessageId.rawValue] as? String)
            .flatMap { UUID(uuidString: $0) }
        let replyText = dict[WatchPayloadKey.replyText.rawValue] as? String
        let ts = (dict["ts"] as? TimeInterval) ?? Date().timeIntervalSince1970
        return WatchActionEnvelope(
            action: action,
            idempotencyId: idempotencyId,
            conversationId: conversationId,
            canonicalMessageId: canonicalMessageId,
            jobId: jobId,
            clientMessageId: clientMessageId,
            replyText: replyText,
            sentAt: Date(timeIntervalSince1970: ts)
        )
    }
}

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

extension WatchConnectivityCoordinator: WCSessionDelegate {
    nonisolated func session(_ session: WCSession, activationDidCompleteWith activationState: WCSessionActivationState, error: Error?) {
        Task { @MainActor in
            self.isReachable = session.isReachable
            if let error {
                self.lastError = error.localizedDescription
            }
        }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any]) {
        // iOS may send us an acknowledgement cold (e.g. reply delivered while
        // the Watch was backgrounded). Treat identically to a reply handler.
        Task { @MainActor in self.handleReply(message) }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any], replyHandler: @escaping ([String: Any]) -> Void) {
        Task { @MainActor in self.handleReply(message) }
        replyHandler([:])
    }

    nonisolated func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        // Queued transferUserInfo delivery from iOS — handle the same way.
        Task { @MainActor in self.handleReply(userInfo) }
    }

    nonisolated func sessionReachabilityDidChange(_ session: WCSession) {
        Task { @MainActor in self.isReachable = session.isReachable }
    }
}
