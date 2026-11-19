//
//  WatchPhoneRelay.swift
//  KallistiWatch
//
//  The relay path: the wrist hands an action to the phone over
//  WatchConnectivity and the phone does the talking. It is the primary path
//  because Tailscale does not run on watchOS, so the wrist often cannot reach
//  the daemon at all.
//
//  What it can do today is carry a turn request and a stop. It cannot carry
//  frames or audio back: the phone side does not pump a turn's stream over the
//  session yet, and pretending otherwise would hide a gap the wrist shows on
//  screen as `relay_no_frames`.
//

import Foundation
import WatchConnectivity

/// Sends one action to the phone and waits for its acknowledgement.
struct WatchPhoneRelay: WatchTransport {
    /// How long an acknowledgement may take before the caller should fall back.
    static let acknowledgementTimeout: Duration = .seconds(2)

    var name: TransportName { .relay }

    func pair(deviceName: String) async throws -> WatchPairing {
        // Only the phone mints a watch pairing, and it hands the result over in
        // the application context.
        throw WatchTransportError.refused(status: 501, code: "pair_from_phone")
    }

    func sendTurn(text: String, conversationId: UUID?) async throws -> String {
        let envelope = WatchActionEnvelope(
            action: .reply,
            idempotencyId: UUID(),
            conversationId: conversationId,
            canonicalMessageId: nil,
            jobId: nil,
            clientMessageId: UUID(),
            replyText: text,
            sentAt: Date()
        )
        try await deliver(envelope)
        // The phone acknowledges a submission, not a job id, so the handle this
        // transport returns is the idempotency id it will be de-duplicated by.
        return envelope.idempotencyId.uuidString
    }

    func cancel(jobID: String) async throws {
        let envelope = WatchActionEnvelope(
            action: .stop,
            idempotencyId: UUID(),
            conversationId: nil,
            canonicalMessageId: nil,
            jobId: UUID(uuidString: jobID),
            clientMessageId: nil,
            replyText: nil,
            sentAt: Date()
        )
        try await deliver(envelope)
    }

    func speak(text: String, voice: String?) async throws -> Data {
        throw WatchTransportError.framesUnavailable
    }

    func transcribe(audio: Data) async throws -> String {
        // The phone does not relay clips yet; the wrist can reach the daemon
        // directly when it needs a transcript.
        throw WatchTransportError.framesUnavailable
    }

    func events(jobID: String, audio: Bool) -> AsyncThrowingStream<WatchFrame, Error> {
        AsyncThrowingStream { continuation in
            continuation.finish(throwing: WatchTransportError.framesUnavailable)
        }
    }

    /// Hand the envelope over, immediate when reachable and queued otherwise.
    private func deliver(_ envelope: WatchActionEnvelope) async throws {
        let session = WCSession.default
        guard session.activationState == .activated else {
            throw WatchTransportError.unreachable
        }
        if session.isReachable {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                session.sendMessage(envelope.toWCDictionary()) { reply in
                    let status = (reply[WatchPayloadKey.acknowledgementStatus.rawValue] as? String)
                        .flatMap(WatchAckStatus.init(rawValue:))
                    switch status {
                    case .accepted, .duplicate:
                        continuation.resume()
                    case .unavailable:
                        continuation.resume(throwing: WatchTransportError.unreachable)
                    default:
                        continuation.resume(
                            throwing: WatchTransportError.refused(status: 409, code: status?.rawValue)
                        )
                    }
                } errorHandler: { error in
                    continuation.resume(throwing: error)
                }
            }
            return
        }
        // Not reachable: queue it. The phone picks it up on the next reachable
        // moment, and the wrist shows it as queued rather than delivered.
        session.transferUserInfo(envelope.toWCDictionary())
    }
}
