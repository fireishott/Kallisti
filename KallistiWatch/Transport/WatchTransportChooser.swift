//
//  WatchTransportChooser.swift
//  KallistiWatch
//
//  Which path carries a turn. The relay goes first because it is the only one
//  that works away from the host's network; the direct client gets the turn when
//  the relay does not answer inside the acknowledgement window. The chooser
//  reports the carrier it used, and the screen says so.
//

import Foundation

/// Picks a transport per turn and remembers nothing: the caller keeps the name.
struct WatchTransportChooser: Sendable {
    let relay: (any WatchTransport)?
    let direct: (any WatchTransport)?
    var acknowledgementTimeout: Duration = WatchPhoneRelay.acknowledgementTimeout

    init(
        relay: (any WatchTransport)?,
        direct: (any WatchTransport)?,
        acknowledgementTimeout: Duration = WatchPhoneRelay.acknowledgementTimeout
    ) {
        self.relay = relay
        self.direct = direct
        self.acknowledgementTimeout = acknowledgementTimeout
    }

    /// Start one turn, and say which transport carried it.
    func sendTurn(
        text: String,
        conversationId: UUID? = nil
    ) async throws -> (handle: String, carrier: TransportName) {
        var relayFailure: Error?
        if let relay {
            do {
                let handle = try await Self.withTimeout(acknowledgementTimeout) {
                    try await relay.sendTurn(text: text, conversationId: conversationId)
                }
                return (handle, .relay)
            } catch {
                relayFailure = error
            }
        }
        if let direct {
            let handle = try await direct.sendTurn(text: text, conversationId: conversationId)
            return (handle, .direct)
        }
        if let relayFailure {
            throw relayFailure
        }
        throw WatchTransportError.notPaired
    }

    /// Run `operation`, or give up after `duration` and say so.
    static func withTimeout<T: Sendable>(
        _ duration: Duration,
        operation: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(for: duration)
                throw WatchTransportError.timedOut
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else {
                throw WatchTransportError.timedOut
            }
            return first
        }
    }
}
