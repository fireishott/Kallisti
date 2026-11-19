//
//  WatchTransport.swift
//  KallistiWatch
//
//  What a wrist needs from a host, stated once. Two transports implement it:
//  the phone relay (WatchConnectivity) and a direct HTTP client for the
//  daemon. The chooser decides which one carries a turn.
//

import Foundation

/// Which path carried the last turn, in words a person can read on screen.
enum TransportName: String, Sendable, CaseIterable {
    case relay
    case direct
    case none
}

/// One frame of a turn, as the watch needs it.
enum WatchFrame: Sendable, Equatable {
    case start(source: String)
    case delta(text: String)
    case tool(name: String)
    case clause(index: Int, wav: Data, text: String)
    case done(text: String)
    case failed(code: String, message: String)
    case cancelled
}

/// Everything the watch asks a host to do, and nothing else.
protocol WatchTransport: Sendable {
    /// The name this transport reports on screen.
    var name: TransportName { get }

    /// Ask the phone to pair this wrist. Only the relay can do this.
    func pair(deviceName: String) async throws -> WatchPairing

    /// Start one turn. Resolves once the host has accepted it, which is the
    /// acknowledgement the chooser races against its timeout.
    func sendTurn(text: String, conversationId: UUID?) async throws -> String

    /// Follow one turn.
    func events(jobID: String, audio: Bool) -> AsyncThrowingStream<WatchFrame, Error>

    /// Speak one answer without a turn, when a screen needs audio alone.
    func speak(text: String, voice: String?) async throws -> Data

    /// Turn a recorded clip into text. The wrist has no `Speech` framework, so
    /// this is the host's whisper answering.
    func transcribe(audio: Data) async throws -> String

    /// Stop one turn.
    func cancel(jobID: String) async throws
}

/// Where a host is, and the tokens that reach it.
struct WatchPairing: Sendable, Equatable {
    let accessToken: String
    let refreshToken: String
    /// The daemon origin, when the phone handed one over.
    let directOrigin: URL?
    /// The relay origin, when the operator configures one.
    let relayOrigin: URL?
    let deviceID: String
}

/// A transport failure with a word the screen can show.
enum WatchTransportError: Error, Equatable {
    case notPaired
    case unreachable
    case refused(status: Int, code: String?)
    case framesUnavailable
    case timedOut

    /// The word a failure states instead of "something went wrong".
    var word: String {
        switch self {
        case .notPaired: "not_paired"
        case .unreachable: "unreachable"
        case let .refused(status, _): "refused_\(status)"
        case .framesUnavailable: "relay_no_frames"
        case .timedOut: "timed_out"
        }
    }
}
