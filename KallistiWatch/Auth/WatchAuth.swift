//
//  WatchAuth.swift
//  KallistiWatch
//
//  One refresh per 401, and the truth on the second: the wrist cannot mint a
//  pairing of its own, so a refresh that fails twice means the phone has to pair
//  it again and the screen says so.
//

import Foundation

/// Decides what a 401 means: refresh once, or give up and ask for a pairing.
struct WatchAuth: Sendable {
    /// Trade a refresh token for a new access token.
    let refresh: @Sendable (String) async throws -> WatchCredentials
    /// Where the pairing lives.
    let store: WatchPairingStore

    /// Run `request`; on a 401, refresh once and run it again. A second 401, or
    /// a refused refresh, is `reauth`: no third attempt, no silent retry loop.
    func run<T: Sendable>(
        _ request: @Sendable (String) async throws -> T
    ) async throws -> T {
        let credentials = try store.credentials()
        guard let credentials else { throw WatchAuthError.notPaired }
        do {
            return try await request(credentials.accessToken)
        } catch WatchTransportError.refused(status: 401, _) {
            let renewed: WatchCredentials
            do {
                renewed = try await refresh(credentials.refreshToken)
            } catch let error as WatchTransportError {
                if case let .refused(status, _) = error {
                    throw WatchAuthError.refreshRefused(status: status)
                }
                throw error
            }
            try store.save(renewed)
            do {
                return try await request(renewed.accessToken)
            } catch WatchTransportError.refused(status: 401, _) {
                throw WatchAuthError.reauth
            }
        }
    }
}
