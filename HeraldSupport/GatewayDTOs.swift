//
//  GatewayDTOs.swift
//  HeraldSupport
//
//  Wire DTOs for the gateway-control plane (`/gw/...` host-root routes).
//  Mirrors the connector's `connector/src/herald_connector/http_facade.py`
//  route registrations so the typed decoder in `GatewayControlClient`
//  matches the server contract exactly.  Envelopes use the connector's
//  rule: payload has a top-level `data` key → envelope decoded, otherwise
//  raw; restart-operation bodies always carry an `error` key (null while
//  healthy) and pass through un-enveloped.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation

// MARK: - Status

/// Response from `GET /gw/status`.  Carries the connector's `gateway-health-v2`
/// payload (see `connector/src/herald_connector/http_facade.py:2732`).
/// Field set is intentionally narrow — the Control Center widget only needs
/// the user-visible facts.  Unrecognized fields are preserved in
/// `connector` / `hermes` / `host` dictionaries so a future widget can
/// surface them without a binary update.
public struct GatewayStatus: Decodable, Sendable, Equatable {
    public let overall: String
    public let connectorConnected: Bool
    public let connectorVersion: String
    public let activeModel: String?
    public let profile: String?
    public let hermesActive: Bool?
    public let activeJobs: Int
    public let uptimeSeconds: Int?
    public let sampledAt: String?

    public init(
        overall: String,
        connectorConnected: Bool,
        connectorVersion: String,
        activeModel: String?,
        profile: String?,
        hermesActive: Bool?,
        activeJobs: Int,
        uptimeSeconds: Int?,
        sampledAt: String?
    ) {
        self.overall = overall
        self.connectorConnected = connectorConnected
        self.connectorVersion = connectorVersion
        self.activeModel = activeModel
        self.profile = profile
        self.hermesActive = hermesActive
        self.activeJobs = activeJobs
        self.uptimeSeconds = uptimeSeconds
        self.sampledAt = sampledAt
    }

    private enum CodingKeys: String, CodingKey {
        case overall
        case connectorConnected
        case connectorVersion
        case activeModel
        case profile
        case hermesActive = "hermesActive"
        case activeJobs
        case uptimeSeconds
        case sampledAt
    }

    public init(from decoder: Decoder) throws {
        let raw = try RawStatus(from: decoder)
        let jobs = raw.jobs?.active ?? 0
        self.init(
            overall: raw.overall ?? "unknown",
            connectorConnected: raw.connectorConnected ?? true,
            connectorVersion: raw.connectorVersion ?? "0.0.0",
            activeModel: raw.hermes?.activeModel,
            profile: raw.hermes?.profile,
            hermesActive: raw.hermes?.state.map { $0 == "healthy" },
            activeJobs: jobs,
            uptimeSeconds: raw.connector?.uptimeSeconds ?? raw.hermes?.uptimeSeconds,
            sampledAt: raw.sampledAt
        )
    }

    /// Wide-shape raw decoder — the connector returns a much richer payload
    /// than Controls needs, so we accept the full set and project the
    /// user-visible subset above.
    private struct RawStatus: Decodable {
        let overall: String?
        let connectorConnected: Bool?
        let connectorVersion: String?
        let sampledAt: String?
        let connector: Connector?
        let hermes: Hermes?
        let jobs: Jobs?

        struct Connector: Decodable {
            let state: String?
            let version: String?
            let uptimeSeconds: Int?
        }

        struct Hermes: Decodable {
            let state: String?
            let activeModel: String?
            let profile: String?
            let uptimeSeconds: Int?
        }

        struct Jobs: Decodable {
            let active: Int?
        }
    }
}

// MARK: - Restart

/// Phase of a `POST /gw/restart` operation.  Mirrors
/// `connector/src/herald_connector/restart_operations.py:RESTART_PHASES`.
public enum RestartPhase: String, Codable, Sendable, Equatable {
    case accepted
    case stopping
    case starting
    case verifying
    case healthy
    case failed
}

/// Single check reported inside a `RestartOperation`.  Mirrors the
/// `RestartCheck` dataclass in `connector/src/herald_connector/restart_operations.py`.
public struct RestartCheck: Codable, Sendable, Equatable, Hashable {
    public let name: String
    public let passed: Bool
    public let detail: String

    public init(name: String, passed: Bool, detail: String) {
        self.name = name
        self.passed = passed
        self.detail = detail
    }
}

/// Optional structured error returned with a `RestartOperation` once it has
/// reached `phase == .failed`.
public struct RestartErrorDetail: Codable, Sendable, Equatable {
    public let stage: String
    public let unit: String?
    public let retryable: Bool?
    public let action: String?

    public init(stage: String, unit: String?, retryable: Bool?, action: String?) {
        self.stage = stage
        self.unit = unit
        self.retryable = retryable
        self.action = action
    }
}

/// Response body from `POST /gw/restart` and `GET /gw/restart/{operationId}`.
/// Mirrors `connector/tests/fixtures/restart/operation_*.json` and the
/// `RestartOperation` dataclass in
/// `connector/src/herald_connector/restart_operations.py`.
public struct RestartOperation: Codable, Sendable, Equatable {
    public let operationId: String
    public let target: String
    public let unit: String
    public let phase: RestartPhase
    public let acceptedAt: String
    public let completedAt: String?
    public let checks: [RestartCheck]
    public let error: RestartErrorDetail?

    public init(
        operationId: String,
        target: String,
        unit: String,
        phase: RestartPhase,
        acceptedAt: String,
        completedAt: String?,
        checks: [RestartCheck],
        error: RestartErrorDetail?
    ) {
        self.operationId = operationId
        self.target = target
        self.unit = unit
        self.phase = phase
        self.acceptedAt = acceptedAt
        self.completedAt = completedAt
        self.checks = checks
        self.error = error
    }
}

/// Preflight snapshot from `GET /gw/restart/preflight?target=…`.  The
/// caller must echo `preflightVersion` back with the restart request;
/// the connector rejects a stale version with 409 PREFLIGHT_STALE.
public struct RestartPreflight: Codable, Sendable, Equatable {
    public let target: String
    public let profile: String?
    public let unit: String
    public let preflightVersion: String
    public let canRestart: Bool
    public let blocker: String?
    public let activeWork: ActiveWork?

    public struct ActiveWork: Codable, Sendable, Equatable {
        public let running: Int
        public let queued: Int
        public let voice: Int
        public let tools: Int

        public init(running: Int, queued: Int, voice: Int, tools: Int) {
            self.running = running
            self.queued = queued
            self.voice = voice
            self.tools = tools
        }
    }

    public init(
        target: String,
        profile: String?,
        unit: String,
        preflightVersion: String,
        canRestart: Bool,
        blocker: String?,
        activeWork: ActiveWork?
    ) {
        self.target = target
        self.profile = profile
        self.unit = unit
        self.preflightVersion = preflightVersion
        self.canRestart = canRestart
        self.blocker = blocker
        self.activeWork = activeWork
    }
}

/// Request body for `POST /gw/restart`.
public struct RestartSubmission: Encodable, Sendable, Equatable {
    public let target: String
    public let preflightVersion: String

    public init(target: String, preflightVersion: String) {
        self.target = target
        self.preflightVersion = preflightVersion
    }
}

// MARK: - Model switch

/// Response from `POST /gw/model/switch`.  Mirrors
/// `connector/src/herald_connector/http_facade.py:switch_model`.  The
/// envelope middleware wraps the body with `{"data": …}` because the
/// payload does not carry a top-level `error` key.
public struct ModelSwitchResult: Decodable, Sendable, Equatable {
    public let switched: Bool
    public let model: String?
    public let error: String?

    public init(switched: Bool, model: String?, error: String?) {
        self.switched = switched
        self.model = model
        self.error = error
    }

    private enum CodingKeys: String, CodingKey {
        case switched, model, error
    }
}
