//
//  WatchContracts.swift
//  Herald
//
//  The wire contract between the phone and the wrist: one envelope, one
//  action set, one set of payload keys, one acknowledgement vocabulary.
//
//  Both targets compile this file on purpose. The envelope is not a
//  phone-only or a watch-only idea, and a second copy is how the two sides
//  drift into disagreeing about a key name.
//

import Foundation

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

/// The pairing the phone hands its wrist, so the watch can talk to the same
/// daemon without a second ceremony. Sent as an application context, which
/// WatchConnectivity delivers again on the next launch if the wrist was asleep.
struct WatchPairingHandoff: Codable, Sendable, Equatable {
    var baseURL: String
    var accessToken: String
    var refreshToken: String

    enum Key {
        static let kind = "kind"
        static let baseURL = "baseURL"
        static let accessToken = "accessToken"
        static let refreshToken = "refreshToken"
        static let kindValue = "pairing"
    }

    /// What travels over WatchConnectivity. Plain strings, because an
    /// application context is a property list.
    var context: [String: Any] {
        [
            Key.kind: Key.kindValue,
            Key.baseURL: baseURL,
            Key.accessToken: accessToken,
            Key.refreshToken: refreshToken,
        ]
    }

    init(baseURL: String, accessToken: String, refreshToken: String) {
        self.baseURL = baseURL
        self.accessToken = accessToken
        self.refreshToken = refreshToken
    }

    init?(context: [String: Any]) {
        guard context[Key.kind] as? String == Key.kindValue,
              let baseURL = context[Key.baseURL] as? String, !baseURL.isEmpty,
              let accessToken = context[Key.accessToken] as? String, !accessToken.isEmpty,
              let refreshToken = context[Key.refreshToken] as? String, !refreshToken.isEmpty
        else { return nil }
        self.baseURL = baseURL
        self.accessToken = accessToken
        self.refreshToken = refreshToken
    }

    /// The watch's own credential type, with the device id the phone is using.
    func credentials(deviceID: String) -> WatchCredentials {
        WatchCredentials(
            accessToken: accessToken,
            refreshToken: refreshToken,
            deviceID: deviceID,
            origin: URL(string: baseURL)
        )
    }
}
