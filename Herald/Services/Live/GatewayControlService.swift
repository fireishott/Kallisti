import Foundation
import os

// MARK: - Gateway restart contract DTOs (Build 33)

/// Preflight response from `GET /gw/restart/preflight?target=hermes`.
/// Mirrors connector/tests/fixtures/restart/preflight_ok.json — the payload
/// arrives inside the relay envelope (`{"data": …}`) because it contains no
/// `error` key.
struct RestartPreflight: Codable, Sendable {
    let target: String
    let profile: String
    let unit: String
    let preflightVersion: String
    let activeWork: ActiveWork?
    let canRestart: Bool
    let blocker: String?
    let observed: ObservedState?

    /// Total in-flight work the gateway reported at preflight time. Shown in
    /// the confirmation dialog so the user knows what a restart interrupts.
    var activeRequestCount: Int {
        guard let activeWork else { return 0 }
        return activeWork.running + activeWork.queued + activeWork.voice + activeWork.tools
    }
}

struct ActiveWork: Codable, Sendable {
    let running: Int
    let queued: Int
    let voice: Int
    let tools: Int
}

struct ObservedState: Codable, Sendable {
    let mainPid: Int?
    let execMainStartTimestamp: String?
    let gatewayState: String?
}

/// Operation response from `POST /gw/restart` and `GET /gw/restart/{id}`.
/// Mirrors connector/tests/fixtures/restart/operation_*.json. These bodies
/// always carry an `error` key (null while healthy), so the connector's
/// envelope middleware passes them through UN-enveloped — decoding is
/// raw-first with the envelope as fallback (see RelayAPIClient).
struct RestartOperation: Codable, Identifiable, Sendable {
    let operationId: String
    let target: String
    let unit: String
    let phase: RestartPhase
    let acceptedAt: String
    let completedAt: String?
    let checks: [RestartCheck]
    let error: RestartErrorDetail?

    var id: String { operationId }

    /// Client-side failure placeholder (preflight fetch error, timeout,
    /// transport failure) so the settings UI has ONE typed error path and
    /// never surfaces `DecodingError` or raw server dicts.
    static func localFailure(
        stage: String,
        unit: String? = nil,
        action: String?,
        retryable: Bool = true
    ) -> RestartOperation {
        let now = ISO8601DateFormatter().string(from: Date())
        return RestartOperation(
            operationId: "local-failure",
            target: "hermes",
            unit: unit ?? "",
            phase: .failed,
            acceptedAt: now,
            completedAt: now,
            checks: [],
            error: RestartErrorDetail(
                stage: stage,
                unit: unit ?? "",
                exitStatus: nil,
                journalExcerpt: nil,
                retryable: retryable,
                action: action
            )
        )
    }

    /// Synthesized healthy result for legacy connectors (no operation
    /// tracking) verified through `GET /gw/health`.
    static func healthVerified(unit: String) -> RestartOperation {
        let now = ISO8601DateFormatter().string(from: Date())
        return RestartOperation(
            operationId: "",
            target: "hermes",
            unit: unit,
            phase: .healthy,
            acceptedAt: now,
            completedAt: now,
            checks: [
                RestartCheck(name: "hermes-ready", passed: true, detail: "health check passed"),
            ],
            error: nil
        )
    }
}

enum RestartPhase: String, Codable, Sendable {
    case accepted, stopping, starting, verifying, healthy, failed
}

struct RestartCheck: Codable, Identifiable, Sendable {
    let name: String
    let passed: Bool
    let detail: String

    var id: String { name }
}

struct RestartErrorDetail: Codable, Sendable {
    let stage: String
    let unit: String
    let exitStatus: Int?
    let journalExcerpt: String?
    let retryable: Bool
    let action: String?
}

/// Health report from `GET /gw/health` — mirrors
/// connector/tests/fixtures/restart/health_report.json. Used to verify a
/// restart when the connector has no operation tracking (legacy path).
struct GatewayHealth: Codable, Sendable {
    let relayConnected: Bool?
    let connectorConnected: Bool?
    let hermesConnected: Bool?
    let hermesReady: Bool?
    let dashboardAvailable: Bool?
    let modelCatalogAvailable: Bool?
    let sessionRoundtripOk: Bool?
    let profile: String?
    let unit: String?
    let mainPid: Int?
    let execMainStartTimestamp: String?
    let connectorVersion: String?
    let uptimeSeconds: Int?
}

/// Request body for `POST /gw/restart` in the Build 33 flow. The connector
/// keys idempotency off the `Idempotency-Key` header plus the accepted
/// `preflightVersion`.
struct RestartSubmission: Encodable, Sendable {
    let target: String
    let preflightVersion: String
}

/// Legacy response shape from the pre-Build-33 connector's `POST /gw/restart`
/// (no operation tracking, no Idempotency-Key). PRESERVED as a backward-compat
/// decoder: the old-style call still works against connectors that have not
/// deployed the restart-operation endpoints, and this decoder recognizes its
/// answer instead of crashing the new flow.
struct RestartResponse: Codable, Sendable {
    let restarting: Bool
    let target: String
    let message: String?
    let error: String?
}

// MARK: - Errors

/// Fired by the fixed `withTimeout` race in SettingsScreen. Declared here so
/// the throwing task-group helper and the UI share one type.
struct TimeoutError: Error, Sendable {}

enum GatewayControlError: LocalizedError, Sendable {
    /// The connector answered 409 CONFLICT with the standard error envelope —
    /// the preflight the user confirmed is stale. Re-fetch and re-confirm.
    case stalePreflight(message: String)
    /// Legacy connector answered but explicitly refused the restart.
    case restartRejected(message: String)
    /// Polling exceeded the deadline without reaching a terminal phase.
    case timeout(seconds: Double)
    /// The poll task was cancelled (e.g. the restart was superseded).
    case cancelled
    /// A restart was accepted but cannot be verified (legacy connector with
    /// no operation tracking AND no health endpoint).
    case verificationUnavailable(message: String)

    var errorDescription: String? {
        switch self {
        case .stalePreflight(let message):
            return message.isEmpty ? "The gateway state changed. Confirm the restart again." : message
        case .restartRejected(let message):
            return message
        case .timeout(let seconds):
            return "The restart did not complete within \(Int(seconds)) seconds."
        case .cancelled:
            return "The restart check was interrupted."
        case .verificationUnavailable(let message):
            return message
        }
    }
}

// MARK: - Service

/// Orchestrates the restart-safe, confirmed, observable, recoverable Hermes
/// gateway restart flow:
///
/// 1. `fetchPreflight(target:)` — snapshot the gateway state (unit, profile,
///    active work, PID) before touching anything.
/// 2. `submitRestart(target:preflight:)` — idempotent POST with an
///    `Idempotency-Key` header and the accepted `preflightVersion`. A 409
///    restart-in-progress conflict returns the EXISTING operation (fixture
///    `conflict_in_progress.json`) so the client joins it instead of starting
///    a second restart; a stale-preflight 409 throws
///    `GatewayControlError.stalePreflight` and the UI re-fetches + re-confirms.
/// 3. `pollUntilTerminal(operation:)` — polls `GET /gw/restart/{id}` until
///    `healthy`/`failed`, publishing each tick through `currentOperation`
///    (@Observable) so the UI shows phase + check progress live. Legacy
///    connectors (no operation tracking) verify via `GET /gw/health`.
///
/// All types are Sendable; the service is MainActor-isolated like the rest of
/// the store layer.
@MainActor
@Observable
final class GatewayControlService {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "GatewayControl")

    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @MainActor () async -> String?
    /// Build 127: whether the current session authenticates via the gateway
    /// session cookie (basic / kallisti-pairing login) rather than a stored
    /// bearer. In cookie-auth mode there IS no bearer token (login deletes
    /// the keychain access token), so requiring one threw "Not authenticated
    /// — please pair your device first" and the restart button never worked
    /// for basic/pairing logins. The relay client rides the cookie in
    /// URLSession.shared when no Authorization header is set.
    private let usesCookieAuth: @MainActor () async -> Bool

    /// Latest operation state, published for observers (settings progress
    /// card, chat suspension). nil until the first submit.
    private(set) var currentOperation: RestartOperation?

    init(
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping @MainActor () async -> String?,
        usesCookieAuth: @escaping @MainActor () async -> Bool = { false }
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.usesCookieAuth = usesCookieAuth
    }

    /// True while a submitted restart has not reached a terminal phase.
    var isRestartInProgress: Bool {
        guard let operation = currentOperation else { return false }
        switch operation.phase {
        case .accepted, .stopping, .starting, .verifying: return true
        case .healthy, .failed: return false
        }
    }

    // MARK: Preflight

    func fetchPreflight(target: String) async throws -> RestartPreflight {
        let token = await accessTokenProvider()
        // Build 107: check for nil/empty token before making the request.
        // Passing nil causes an unauthenticated request which returns 401.
        // Build 127: except in cookie-auth mode (basic/pairing login), where
        // there is no bearer token and the session cookie authenticates.
        let cookieAuth = await usesCookieAuth()
        let hasToken = token.map { !$0.isEmpty } ?? false
        guard cookieAuth || hasToken else {
            throw GatewayControlError.restartRejected(
                message: "Not authenticated — please pair your device first."
            )
        }
        return try await apiClient.getGatewayControl(
            path: "gw/restart/preflight?target=\(target)",
            accessToken: hasToken ? token : nil
        )
    }

    // MARK: Restart submission

    func submitRestart(target: String, preflight: RestartPreflight) async throws -> RestartOperation {
        let token = await accessTokenProvider()
        // Build 107: check for nil/empty token before making the request.
        // Build 127: cookie-auth mode (basic/pairing login) has no bearer —
        // the session cookie authenticates.
        let cookieAuth = await usesCookieAuth()
        let hasToken = token.map { !$0.isEmpty } ?? false
        guard cookieAuth || hasToken else {
            throw GatewayControlError.restartRejected(
                message: "Not authenticated — please pair your device first."
            )
        }
        let result: RelayAPIClient.GatewayRestartResult
        do {
            result = try await apiClient.postGatewayRestart(
                body: RestartSubmission(target: target, preflightVersion: preflight.preflightVersion),
                accessToken: hasToken ? token : nil,
                idempotencyKey: UUID().uuidString
            )
        } catch let RelayAPIClient.ClientError.serverError(_, message, _, status) where status == 409 {
            // The 409 body was NOT an existing operation (that path is handled
            // inside the client) — so the preflight we confirmed is stale.
            throw GatewayControlError.stalePreflight(message: message)
        }

        let operation: RestartOperation
        switch result {
        case .operation(let op):
            operation = op
        case .legacy(let response):
            guard response.restarting else {
                throw GatewayControlError.restartRejected(
                    message: response.error ?? response.message ?? "The gateway rejected the restart."
                )
            }
            // Pre-Build-33 connector: restart accepted but there is no
            // operation to poll. Synthesize an accepted operation with an
            // empty operationId — pollUntilTerminal then verifies via
            // GET /gw/health instead.
            Self.logger.info("Legacy restart response (no operation tracking) — will verify via health")
            operation = RestartOperation(
                operationId: "",
                target: target,
                unit: preflight.unit,
                phase: .accepted,
                acceptedAt: ISO8601DateFormatter().string(from: Date()),
                completedAt: nil,
                checks: [],
                error: nil
            )
        }
        currentOperation = operation
        return operation
    }

    // MARK: Operation polling

    func fetchOperation(_ operationID: String) async throws -> RestartOperation {
        let token = await accessTokenProvider()
        return try await apiClient.getGatewayControl(
            path: "gw/restart/\(operationID)",
            accessToken: token
        )
    }

    func fetchHealth() async throws -> GatewayHealth {
        let token = await accessTokenProvider()
        return try await apiClient.getGatewayControl(
            path: "gw/health",
            accessToken: token
        )
    }

    /// Polls the operation until a terminal phase, publishing every tick to
    /// `currentOperation`. Transient transport failures (gateway is literally
    /// going down/coming back mid-restart) are retried until `deadline`;
    /// a 404 on the operation means no tracking exists (legacy connector) and
    /// verification switches to `GET /gw/health`.
    func pollUntilTerminal(
        operation: RestartOperation,
        pollInterval: Duration = .seconds(2),
        deadline: Duration = .seconds(120)
    ) async throws -> RestartOperation {
        let startedAt = ContinuousClock.now
        let deadlineSeconds = Double(deadline.components.seconds)
        let operationID = operation.operationId
        var useHealthFallback = operationID.isEmpty

        while !Task.isCancelled {
            if startedAt.duration(to: .now) > deadline {
                throw GatewayControlError.timeout(seconds: deadlineSeconds)
            }

            if useHealthFallback {
                if try await healthReportsReady() {
                    let verified = RestartOperation.healthVerified(unit: operation.unit)
                    currentOperation = verified
                    return verified
                }
            } else {
                do {
                    let fetched = try await fetchOperation(operationID)
                    currentOperation = fetched
                    if fetched.phase == .healthy || fetched.phase == .failed {
                        return fetched
                    }
                } catch let RelayAPIClient.ClientError.serverError(_, _, _, status) where status == 404 {
                    // Operation is not tracked (legacy connector) — verify via health.
                    Self.logger.info("Operation \(operationID.prefix(8)) not found — falling back to health verification")
                    useHealthFallback = true
                } catch {
                    // Transient: the gateway is restarting, so requests may
                    // fail for a while. Keep polling until the deadline.
                    Self.logger.warning("Operation poll failed (retrying): \(error.localizedDescription)")
                }
            }

            try? await Task.sleep(for: pollInterval)
        }
        throw GatewayControlError.cancelled
    }

    /// Single health probe: `true` once the gateway reports hermes ready.
    /// Transient failures (gateway mid-restart) return `false` and are retried
    /// by the caller until the deadline. A 404 means the connector has no
    /// health endpoint at all — verification is impossible, so surface that
    /// immediately instead of polling it out.
    private func healthReportsReady() async throws -> Bool {
        do {
            let health = try await fetchHealth()
            return health.hermesReady == true
        } catch let RelayAPIClient.ClientError.serverError(_, _, _, status) where status == 404 {
            throw GatewayControlError.verificationUnavailable(
                message: "The restart was accepted, but this connector cannot report restart status. Check the Hermes gateway logs on the host."
            )
        } catch {
            // Gateway is mid-restart — health probes fail until it comes back.
            Self.logger.warning("Health probe failed (retrying): \(error.localizedDescription)")
            return false
        }
    }

    func reset() {
        currentOperation = nil
    }
}
