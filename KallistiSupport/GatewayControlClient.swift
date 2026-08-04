//
//  GatewayControlClient.swift
//  HeraldSupport
//
//  Narrow HTTP client for the gateway-control plane.  Deliberately
//  smaller than `Herald/Services/Support/RelayAPIClient.swift` — Controls
//  do not need SSE, raw attachments, or the broader /v1/... envelope —
//  just the host-root /gw/... routes that manage the gateway.
//
//  Routes are hard-coded to the connector's registered paths (see the
//  sanitized route table under `$EVIDENCE_ROOT/tests/gateway-route-table.md`).
//  Tests inject a `URLSession` and a configuration + credential provider
//  so a fake URLProtocol can exercise URL construction, idempotency,
//  409 handling, and typed-error mapping end to end.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation
import os

/// Provider for the current relay base URL.  Implemented by
/// `SharedRelayConfiguration` in production and by in-memory fakes in tests.
public protocol RelayConfigurationProviding: Sendable {
    func relayBaseURL() -> String
    var isConfigured: Bool { get }
}

extension SharedRelayConfiguration: RelayConfigurationProviding {}

/// Provider for the current relay access token.  Implemented by
/// `SharedCredentialProvider` in production and by in-memory fakes in tests.
public protocol CredentialProviding: Sendable {
    func accessToken() throws -> String?
    func setAccessToken(_ token: String?) throws
}

extension SharedCredentialProvider: CredentialProviding {}

/// Minimal HTTP client for the gateway-control plane.  Constructed with a
/// relay configuration, a credential provider, and an injectable
/// `URLSession` so tests can substitute a `URLProtocol` mock.
public final class GatewayControlClient: @unchecked Sendable {
    private let configurationProvider: any RelayConfigurationProviding
    private let credentialProvider: any CredentialProviding
    private let session: URLSession
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "GatewayControl")

    public init(
        configuration: any RelayConfigurationProviding,
        credentials: any CredentialProviding,
        session: URLSession = .shared
    ) {
        self.configurationProvider = configuration
        self.credentialProvider = credentials
        self.session = session
    }

    // MARK: - Public API

    /// `GET /gw/status`.  Returns the narrow `GatewayStatus` DTO.
    public func fetchStatus() async throws -> GatewayStatus {
        let request = try buildRequest(method: "GET", path: "gw/status", body: nil)
        return try await send(request: request, decode: { data in
            try Self.decodeGatewayPayload(data, as: GatewayStatus.self)
        })
    }

    /// `GET /gw/restart/preflight?target=hermes` — must be called before
    /// `submitRestart` so the connector can verify the preflightVersion
    /// has not changed since confirmation.
    public func fetchPreflight(target: String) async throws -> RestartPreflight {
        guard Self.validTargets.contains(target) else {
            throw GatewayControlError.unexpected(reason: "Unknown restart target '\(target)'.")
        }
        let request = try buildRequest(
            method: "GET",
            path: "gw/restart/preflight",
            queryItems: [URLQueryItem(name: "target", value: target)],
            body: nil
        )
        return try await send(request: request, decode: { data in
            try Self.decodeGatewayPayload(data, as: RestartPreflight.self)
        })
    }

    /// `POST /gw/restart` with an `Idempotency-Key` header.  A 409 with an
    /// existing operation body is returned as `.conflict(operationId:)` so
    /// the caller can join the in-flight restart instead of starting a
    /// second one.  A 409 with the standard error envelope (PREFLIGHT_STALE)
    /// surfaces as `.conflict(operationId: nil, …)`.
    public func submitRestart(
        target: String,
        preflightVersion: String,
        idempotencyKey: String
    ) async throws -> RestartOperation {
        let body = try JSONEncoder().encode(
            RestartSubmission(target: target, preflightVersion: preflightVersion)
        )
        var request = try buildRequest(
            method: "POST",
            path: "gw/restart",
            body: body
        )
        request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key")

        return try await send(request: request, decode: { data in
            // 409 restart-in-progress bodies are returned raw (they carry
            // an `error` key, so the envelope middleware passes them
            // through).  Decode restart-operation-v1 directly.
            if let op = try? JSONDecoder().decode(RestartOperation.self, from: data) {
                return op
            }
            if let envelope = try? JSONDecoder().decode(
                Envelope<RestartOperation>.self, from: data
            ) {
                return envelope.data
            }
            throw GatewayControlError.unexpected(
                reason: "Unrecognized restart response from the gateway."
            )
        }, conflictOperationIdProvider: { data in
            Self.extractConflictOperationId(data)
        })
    }

    /// `GET /gw/restart/{operationId}` — poll the operation until a
    /// terminal phase is reported.
    public func pollRestart(operationId: String) async throws -> RestartOperation {
        let request = try buildRequest(
            method: "GET",
            path: "gw/restart/\(operationId)",
            body: nil
        )
        return try await send(request: request, decode: { data in
            try Self.decodeGatewayPayload(data, as: RestartOperation.self)
        })
    }

    /// `POST /gw/model/switch` — switched body shape is
    /// `{"data": {"switched": …, "model": …, "error": …}}`.
    public func switchModel(name: String) async throws -> ModelSwitchResult {
        guard !name.isEmpty else {
            throw GatewayControlError.unexpected(reason: "Model name is required.")
        }
        let payload = ["name": name]
        let body = try JSONEncoder().encode(payload)
        let request = try buildRequest(
            method: "POST",
            path: "gw/model/switch",
            body: body
        )
        return try await send(request: request, decode: { data in
            try Self.decodeGatewayPayload(data, as: ModelSwitchResult.self)
        })
    }

    // MARK: - Internals

    /// Valid values for `target=` per the connector's allowlist
    /// (`http_facade.py:gateway_restart`).
    public static let validTargets: Set<String> = ["hermes", "connector"]

    /// Build an authenticated URLRequest targeting the gateway host root.
    /// Strips any `/v1`, `/v2`, ... suffix from the configured base URL
    /// because the gateway routes live at host-root (`/gw/...`), not
    /// under the `/v1` API prefix.
    private func buildRequest(
        method: String,
        path: String,
        queryItems: [URLQueryItem] = [],
        body: Data?
    ) throws -> URLRequest {
        guard configurationProvider.isConfigured else {
            throw GatewayControlError.notConfigured(
                reason: "No paired relay URL — open Herald to pair this device."
            )
        }
        let token = try credentialProvider.accessToken()
        guard let token, !token.isEmpty else {
            throw GatewayControlError.unauthorized(
                reason: "Not authenticated — please pair your device first."
            )
        }

        var urlString = configurationProvider.relayBaseURL().trimmingCharacters(
            in: CharacterSet(charactersIn: "/")
        )
        // Strip the API version prefix (e.g. /v1, /v2) so gateway routes
        // resolve against the host root.
        if let lastSlash = urlString.lastIndex(of: "/") {
            let lastComponent = urlString[urlString.index(after: lastSlash)...]
            if lastComponent.allSatisfy({ $0.isLetter || $0.isNumber }) && lastComponent.hasPrefix("v") {
                urlString = String(urlString[..<lastSlash])
            }
        }

        var components = URLComponents()
        components.scheme = nil
        components.host = nil
        // Reconstruct from the trimmed string so we keep the host + port.
        guard let parsed = URLComponents(string: urlString) else {
            throw GatewayControlError.notConfigured(
                reason: "The relay base URL is not a valid URL."
            )
        }
        var resolved = parsed
        resolved.path = "/" + path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        resolved.queryItems = queryItems.isEmpty ? nil : queryItems

        guard let url = resolved.url else {
            throw GatewayControlError.notConfigured(
                reason: "The relay base URL is not a valid URL."
            )
        }

        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(UUID().uuidString, forHTTPHeaderField: "X-Request-ID")
        request.httpBody = body
        request.timeoutInterval = 30
        return request
    }

    /// Send the request and dispatch the response to the typed decoder.
    /// Throws `GatewayControlError` for non-2xx responses.  The 409
    /// handling reads an `operationId` out of the conflict body when the
    /// decoder was for a restart operation.
    private func send<T: Decodable & Sendable>(
        request: URLRequest,
        decode: (Data) throws -> T,
        conflictOperationIdProvider: (Data) -> String? = { _ in nil }
    ) async throws -> T {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch let error as URLError where error.code == .timedOut {
            throw GatewayControlError.retriable(reason: "The gateway timed out.")
        } catch {
            throw GatewayControlError.retriable(reason: error.localizedDescription)
        }

        guard let http = response as? HTTPURLResponse else {
            throw GatewayControlError.unexpected(reason: "Invalid response from the gateway.")
        }

        switch http.statusCode {
        case 200..<300:
            do {
                return try decode(data)
            } catch {
                throw GatewayControlError.unexpected(
                    reason: "Could not decode the gateway response."
                )
            }
        case 401, 403:
            throw GatewayControlError.unauthorized(reason: Self.sanitizedBody(data))
        case 404:
            throw GatewayControlError.routeMismatch(
                status: 404,
                reason: "The gateway does not expose this route — check the connector version."
            )
        case 409:
            let operationId = conflictOperationIdProvider(data)
            throw GatewayControlError.conflict(
                operationId: operationId,
                reason: Self.sanitizedConflictBody(data, hasOperationId: operationId != nil)
            )
        case 426:
            throw GatewayControlError.routeMismatch(
                status: 426,
                reason: "This Herald build requires a connector update."
            )
        case 500..<600:
            throw GatewayControlError.retriable(
                reason: "The gateway returned HTTP \(http.statusCode)."
            )
        default:
            throw GatewayControlError.unexpected(
                reason: "Unexpected HTTP \(http.statusCode)."
            )
        }
    }

    /// Decode an enveloped or raw payload.  Envelopes are detected by the
    /// presence of a top-level `data` key — the connector's middleware
    /// wraps every body that does NOT carry an `error` key.
    private static func decodeGatewayPayload<T: Decodable>(_ data: Data, as type: T.Type) throws -> T {
        if let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           dict["data"] != nil {
            return try JSONDecoder().decode(Envelope<T>.self, from: data).data
        }
        return try JSONDecoder().decode(type, from: data)
    }

    /// Extract an operation id from a 409 restart-in-progress body.  The
    /// connector emits either the raw operation dict (preferred) or the
    /// error envelope `{"error": {"operationId": …}}`.
    private static func extractConflictOperationId(_ data: Data) -> String? {
        if let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            if let operation = dict["operation"] as? [String: Any],
               let id = operation["operationId"] as? String, !id.isEmpty {
                return id
            }
            if let err = dict["error"] as? [String: Any],
               let id = err["operationId"] as? String, !id.isEmpty {
                return id
            }
        }
        return nil
    }

    /// Returns a short, non-secret reason string for a 4xx response.  Caps
    /// at 200 chars and drops HTML so a misconfigured proxy cannot leak
    /// markup into the dialog.
    private static func sanitizedBody(_ data: Data) -> String {
        guard let text = String(data: data, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines),
            !text.isEmpty,
            text.count <= 200,
            !text.hasPrefix("<")
        else { return "" }
        return text
    }

    /// Same as `sanitizedBody` but distinguishes PREFLIGHT_STALE from a
    /// restart-in-progress conflict.
    private static func sanitizedConflictBody(_ data: Data, hasOperationId: Bool) -> String {
        if hasOperationId { return "" }
        if let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let err = dict["error"] as? [String: Any],
           let message = err["message"] as? String, !message.isEmpty {
            return message
        }
        return ""
    }

    /// Outer-envelope `{ "data": … }`.
    private struct Envelope<T: Decodable>: Decodable {
        let data: T
    }
}
