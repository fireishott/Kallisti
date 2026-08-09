import Foundation
import os

/// Typed feature calls over the native gateway's JSON-RPC/WebSocket channel.
///
/// The pre-0.2.0 feature screens (Gateway Status, model picker, Settings host
/// row, aux models) were built against the OLD connector REST facade
/// (`/gw/status`, `/v1/models`, `/v1/aux`, `hosts/current`). That facade is
/// dead for native clients: the native gateway bearer token is only valid on
/// the gateway (dashboard port 9119), and Caddy routes `/v1/*` to the API
/// server and `/gw/*` to the connector - neither accepts it. Every one of
/// those screens 404'd or threw "The data couldn't be read because it is
/// missing" (a JSON decode of a 401/404 body).
///
/// This client re-targets those screens at gateway JSON-RPC methods that
/// actually exist:
///
///   Gateway Status  -> config.get(key:"provider"), session.active_list,
///                      system.battery, usage.bars
///   Model list      -> model.options              ({providers, model, provider})
///   Model switch    -> slash.exec  "/model <provider>/<name>"
///   Settings row    -> config.get(key:"provider"), config.get(key:"profile")
///   Aux models      -> config.get(key:"full")     (config.auxiliary section)
///
/// It rides the SAME NativeGatewayClient instance the chat client uses (one
/// socket, one ticket). All methods are read-only except switchModel, which
/// routes through the gateway's own model-switch machinery.
struct NativeGatewayFeatureClient {
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "NativeGatewayFeatureClient")

    /// A live, connected NativeGatewayClient (owned by NativeKallistiClient).
    /// The feature client is stateless: every call fetches the current client
    /// so it stays correct across reconnects.
    private let clientProvider: @MainActor () -> NativeGatewayClient?
    private let currentSessionIdProvider: @MainActor () async -> String?

    init(
        clientProvider: @escaping @MainActor () -> NativeGatewayClient?,
        currentSessionIdProvider: @escaping @MainActor () async -> String? = { nil }
    ) {
        self.clientProvider = clientProvider
        self.currentSessionIdProvider = currentSessionIdProvider
    }

    // MARK: - Gateway Status

    /// Host + gateway status for the Gateway Status screen.
    /// Combines the methods that exist on the native gateway (there is no
    /// `/gw/status` equivalent - that was a connector route).
    struct GatewayStatusSnapshot {
        var model: String?
        var provider: String?
        var availableProviders: [String] = []
        var activeSessionCount: Int = 0
        var batteryAvailable: Bool = false
        var batteryPercent: Int?
        var batteryPlugged: Bool = false
        var hermesHome: String?
        var usageAvailable: Bool = false
    }

    func gatewayStatus() async throws -> GatewayStatusSnapshot {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }

        var snapshot = GatewayStatusSnapshot()

        // config.get(key: "provider") -> {model, provider, providers: []}
        do {
            let providerResp = try await client.send(method: "config.get", params: ["key": "provider"])
            if let error = providerResp.error { throw error }
            if let result = providerResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(ProviderConfigResponse.self, from: data) {
                snapshot.model = decoded.model
                snapshot.provider = decoded.provider
                snapshot.availableProviders = (decoded.providers ?? []).compactMap { $0.id }
            }
        } catch {
            Self.logger.error("gatewayStatus config.get failed: \(error.localizedDescription)")
        }

        // session.active_list -> {sessions: [{session_id, ...}]}
        do {
            let activeResp = try await client.send(method: "session.active_list", params: [String: String]())
            if let error = activeResp.error { throw error }
            if let result = activeResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(ActiveSessionsResponse.self, from: data) {
                snapshot.activeSessionCount = decoded.sessions?.count ?? 0
            }
        } catch {
            Self.logger.error("gatewayStatus session.active_list failed: \(error.localizedDescription)")
        }

        // system.battery -> {available, percent, plugged, category}
        do {
            let battResp = try await client.send(method: "system.battery", params: [String: String]())
            if let error = battResp.error { throw error }
            if let result = battResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(BatteryResponse.self, from: data) {
                snapshot.batteryAvailable = decoded.available
                snapshot.batteryPercent = decoded.percent
                snapshot.batteryPlugged = decoded.plugged ?? false
            }
        } catch {
            Self.logger.error("gatewayStatus system.battery failed: \(error.localizedDescription)")
        }

        // config.get(key: "profile") -> {home, display}
        do {
            let profileResp = try await client.send(method: "config.get", params: ["key": "profile"])
            if let error = profileResp.error { throw error }
            if let result = profileResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(ProfileResponse.self, from: data) {
                snapshot.hermesHome = decoded.home
            }
        } catch {
            Self.logger.error("gatewayStatus config.get profile failed: \(error.localizedDescription)")
        }

        // usage.bars -> {ok, available, ...} (fail-open)
        do {
            let usageResp = try await client.send(method: "usage.bars", params: [String: String]())
            if let error = usageResp.error { throw error }
            if let result = usageResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(UsageBarsResponse.self, from: data) {
                snapshot.usageAvailable = decoded.available ?? false
            }
        } catch {
            Self.logger.error("gatewayStatus usage.bars failed: \(error.localizedDescription)")
        }

        return snapshot
    }

    // MARK: - Models

    struct ModelProvider: Decodable, Identifiable, Hashable {
        let slug: String
        let name: String?
        let models: [String]?
        let isUserDefined: Bool?
        let authenticated: Bool?

        var id: String { slug }
        var displayName: String { name ?? slug }
        var modelNames: [String] { models ?? [] }

        enum CodingKeys: String, CodingKey {
            case slug, name, models, authenticated
            case isUserDefined = "is_user_defined"
        }
    }

    private struct ModelOptionsResponse: Decodable {
        let providers: [ModelProvider]?
        let model: String?
        let provider: String?
    }

    /// Active model + provider from model.options (authoritative server state).
    struct ActiveModelInfo {
        var model: String?
        var provider: String?
        var providers: [ModelProvider] = []
    }

    func modelOptions(explicitOnly: Bool = true) async throws -> ActiveModelInfo {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }
        let response = try await client.send(
            method: "model.options",
            params: ["explicit_only": explicitOnly]
        )
        if let error = response.error { throw error }
        guard let result = response.result,
              let data = try? JSONEncoder().encode(result),
              let decoded = try? JSONDecoder().decode(ModelOptionsResponse.self, from: data)
        else {
            return ActiveModelInfo()
        }
        return ActiveModelInfo(
            model: decoded.model,
            provider: decoded.provider,
            providers: decoded.providers ?? []
        )
    }

    /// Switch the active model via the gateway's /model slash machinery.
    /// `provider/model` format is accepted by parse_model_switch_args.
    func switchModel(_ name: String, provider: String) async throws {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }
        let target = provider.isEmpty ? name : "\(provider)/\(name)"
        var params: [String: String] = ["command": "/model \(target)"]
        // slash.exec requires a session_id (_sess_nowait 4001s without one).
        // Scope the switch to the active conversation when there is one so the
        // gateway can resolve the session instead of rejecting the command.
        if let sessionId = await currentSessionIdProvider() {
            params["session_id"] = sessionId
        }
        let response = try await client.send(
            method: "slash.exec",
            params: params
        )
        if let error = response.error { throw error }
    }

    // MARK: - Settings host row

    /// Run a headless hermes CLI command over the gateway WS (cli.exec).
    /// Returns stdout+stderr combined. The gateway allows non-interactive
    /// subcommands (profile list/use, etc.) - blocked commands come back as
    /// `blocked: true` with a hint.
    func cliExec(argv: [String], timeout: Int = 60) async throws -> String {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }
        let response = try await client.send(
            method: "cli.exec",
            params: CliExecParams(argv: argv, timeout: timeout)
        )
        if let error = response.error { throw error }
        guard let result = response.result,
              let data = try? JSONEncoder().encode(result),
              let decoded = try? JSONDecoder().decode(CliExecResponse.self, from: data)
        else {
            throw NativeGatewayClientError.unexpectedFrame
        }
        if decoded.blocked == true {
            throw NativeGatewayClientError.unexpectedFrame
        }
        return decoded.output ?? ""
    }

    private struct CliExecParams: Encodable {
        let argv: [String]
        let timeout: Int
    }

    private struct CliExecResponse: Decodable {
        let blocked: Bool?
        let code: Int?
        let output: String?
    }

    /// Hermes host identity for the Settings host row.
    struct HostInfo {
        var model: String?
        var provider: String?
        var hermesHome: String?
        var hermesHomeDisplay: String?
        var providerCount: Int = 0
    }

    func hostInfo() async throws -> HostInfo {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }
        var info = HostInfo()

        do {
            let providerResp = try await client.send(method: "config.get", params: ["key": "provider"])
            if let error = providerResp.error { throw error }
            if let result = providerResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(ProviderConfigResponse.self, from: data) {
                info.model = decoded.model
                info.provider = decoded.provider
                info.providerCount = decoded.providers?.count ?? 0
            }
        } catch {
            Self.logger.error("hostInfo config.get provider failed: \(error.localizedDescription)")
        }

        do {
            let profileResp = try await client.send(method: "config.get", params: ["key": "profile"])
            if let error = profileResp.error { throw error }
            if let result = profileResp.result,
               let data = try? JSONEncoder().encode(result),
               let decoded = try? JSONDecoder().decode(ProfileResponse.self, from: data) {
                info.hermesHome = decoded.home
                info.hermesHomeDisplay = decoded.display
            }
        } catch {
            Self.logger.error("hostInfo config.get profile failed: \(error.localizedDescription)")
        }

        return info
    }

    // MARK: - Aux models

    /// Auxiliary task overrides from config (vision, web_extract, compression).
    struct AuxTaskInfo {
        var label: String
        var provider: String
        var model: String

        var isOverride: Bool { provider != "auto" || !model.isEmpty }
    }

    func auxiliaryModels() async throws -> [AuxTaskInfo] {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }
        let response = try await client.send(method: "config.get", params: ["key": "full"])
        if let error = response.error { throw error }
        guard let result = response.result,
              let data = try? JSONEncoder().encode(result),
              let decoded = try? JSONDecoder().decode(FullConfigResponse.self, from: data)
        else {
            return []
        }

        let auxiliary = decoded.config?.auxiliary
        let vision = auxiliary?.vision
        let webExtract = auxiliary?.webExtract
        let compression = auxiliary?.compression

        var rows: [AuxTaskInfo] = []
        if let vision {
            rows.append(AuxTaskInfo(
                label: "Vision",
                provider: vision.provider ?? "auto",
                model: vision.model ?? ""
            ))
        }
        if let webExtract {
            rows.append(AuxTaskInfo(
                label: "Web extract",
                provider: webExtract.provider ?? "auto",
                model: webExtract.model ?? ""
            ))
        }
        if let compression {
            rows.append(AuxTaskInfo(
                label: "Compression",
                provider: compression.provider ?? "auto",
                model: compression.model ?? ""
            ))
        }
        return rows
    }

    // MARK: - Response shapes

    private struct ProviderConfigResponse: Decodable {
        struct ProviderInfo: Decodable {
            let id: String?
            let label: String?
            let aliases: [String]?
        }
        let model: String?
        let provider: String?
        // Server sends list[dict] (id/label/aliases from
        // list_available_providers). Decoding as [String] throws a silent
        // typeMismatch, which zeroed providerCount and cascaded into
        // Settings showing "Model: Unavailable / Providers: 0".
        let providers: [ProviderInfo]?
    }

    private struct ProfileResponse: Decodable {
        let home: String?
        let display: String?
    }

    private struct ActiveSessionsResponse: Decodable {
        struct SessionRow: Decodable {
            let sessionId: String?
            enum CodingKeys: String, CodingKey {
                case sessionId = "session_id"
            }
        }
        let sessions: [SessionRow]?
    }

    private struct BatteryResponse: Decodable {
        let available: Bool
        let percent: Int?
        let plugged: Bool?
    }

    private struct UsageBarsResponse: Decodable {
        let ok: Bool?
        let available: Bool?
    }

    private struct FullConfigResponse: Decodable {
        struct AuxiliarySection: Decodable {
            struct TaskConfig: Decodable {
                let provider: String?
                let model: String?
            }
            let vision: TaskConfig?
            let webExtract: TaskConfig?
            let compression: TaskConfig?

            enum CodingKeys: String, CodingKey {
                case vision, compression
                case webExtract = "web_extract"
            }
        }
        struct Config: Decodable {
            let auxiliary: AuxiliarySection?
        }
        let config: Config?
    }
}
