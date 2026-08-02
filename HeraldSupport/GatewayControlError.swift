//
//  GatewayControlError.swift
//  HeraldSupport
//
//  Typed errors raised by `GatewayControlClient`.  The variants match the
//  connector's HTTP failure modes so the control surface can render a
//  truthful dialog without exposing raw server dumps or credentials.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation

/// Typed gateway-control error.  Suitable for `AppIntent` dialogs and
/// widget timelines — every case carries a user-safe description that
/// never embeds the access token or raw response bytes.
public enum GatewayControlError: LocalizedError, Sendable, Equatable {
    /// The caller did not pair the device, or the shared Keychain access
    /// group is unreachable.  Re-pair is required.
    case notConfigured(reason: String)
    /// The relay access token is missing or rejected (HTTP 401/403).
    /// Distinct from "offline" — the gateway is reachable but does not
    /// accept the credential.
    case unauthorized(reason: String)
    /// The HTTP status code maps to a route or protocol mismatch (404 or
    /// 426).  The UI should surface a "this iOS build is incompatible with
    /// the connector" hint.
    case routeMismatch(status: Int, reason: String)
    /// The connector answered 409.  A restart-in-progress 409 carries the
    /// existing operation id; the client joins it instead of starting a
    /// second restart.  A PREFLIGHT_STALE 409 carries no operation id and
    /// the UI must re-fetch the preflight before retrying.
    case conflict(operationId: String?, reason: String)
    /// A retriable transport failure (timeout, 5xx, connection lost).
    /// The UI should show "try again".
    case retriable(reason: String)
    /// Catch-all for unexpected shapes (e.g. decode error against a body
    /// the connector should never have emitted).  Not a user-facing retry.
    case unexpected(reason: String)

    public var errorDescription: String? {
        switch self {
        case .notConfigured(let reason):
            return reason
        case .unauthorized(let reason):
            return reason.isEmpty ? "The relay rejected the access token." : reason
        case .routeMismatch(let status, let reason):
            return reason.isEmpty
                ? "Route/protocol mismatch (HTTP \(status))."
                : reason
        case .conflict(_, let reason):
            return reason.isEmpty
                ? "A restart is already in progress."
                : reason
        case .retriable(let reason):
            return reason.isEmpty
                ? "The gateway did not respond. Try again."
                : reason
        case .unexpected(let reason):
            return reason
        }
    }

    /// True for errors the user can recover from by tapping again.
    public var isRetriable: Bool {
        switch self {
        case .retriable, .unauthorized, .conflict, .unexpected: return true
        case .notConfigured, .routeMismatch: return false
        }
    }
}
