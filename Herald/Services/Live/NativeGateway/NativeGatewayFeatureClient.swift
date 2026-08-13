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
    /// Base URL of the native gateway (http://host:9119). Used to derive the
    /// connector HTTP facade URL (same host, port 8010) for version rows.
    private let gatewayBaseURLProvider: @MainActor () -> String

    init(
        clientProvider: @escaping @MainActor () -> NativeGatewayClient?,
        currentSessionIdProvider: @escaping @MainActor () async -> String? = { nil },
        gatewayBaseURLProvider: @escaping @MainActor () -> String = { "http://localhost:9119" }
    ) {
        self.clientProvider = clientProvider
        self.currentSessionIdProvider = currentSessionIdProvider
        self.gatewayBaseURLProvider = gatewayBaseURLProvider
    }

    // MARK: - Version info (native mode)

    /// Hermes Agent version string from `hermes version` via cli.exec.
    func agentVersion() async throws -> String {
        let raw = try await cliExec(argv: ["version"], timeout: 30)
        guard !raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            Self.logger.warning("agentVersion: empty response from cli.exec")
            return ""
        }
        return raw
    }

    /// Connector version from the HTTP facade /v1/version.
    /// Uses the facade base URL so it works for both LAN hosts (host:8010)
    /// and public relay hosts (Caddy routes /v1/version to the connector).
    /// The endpoint answers unauthenticated.
    func connectorVersion() async -> String? {
        let gatewayBase = await gatewayBaseURLProvider()
        guard let facadeBase = NativeKallistiClient.facadeBaseURL(for: gatewayBase) else { return nil }
        struct VersionEnvelope: Decodable { struct VersionData: Decodable { let version: String? }; let data: VersionData? }
        guard let url = URL(string: facadeBase.hasSuffix("/") ? facadeBase + "v1/version" : facadeBase + "/v1/version") else { return nil }
        var request = URLRequest(url: url); request.timeoutInterval = 8
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
            return try? JSONDecoder().decode(VersionEnvelope.self, from: data).data?.version
        } catch { return nil }
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

    private struct ModelOptionsParams: Encodable {
        let explicitOnly: Bool
        let sessionId: String?

        enum CodingKeys: String, CodingKey {
            case explicitOnly = "explicit_only"
            case sessionId = "session_id"
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
        // LATENCY (build 37): this RPC had no timeout override, inheriting
        // the client's 60s default. ModelStore.switchModel calls
        // modelOptions(force: true) immediately AFTER a successful
        // slash.exec switch to confirm the new active model — if the socket
        // is a phantom (backgrounded app, dead but not-yet-detected), this
        // hangs 60s+ and the picker spinner never clears even though the
        // model already switched server-side (confirmed in gateway logs).
        // The server builds this payload in <1s on a healthy socket, so a
        // 15s bound is generous while still failing fast on a dead one.
        // model.options must inspect the same live session that /model updates.
        // Without this, its response falls back to global config and overwrites
        // the picker with the default model even though the conversation itself
        // is correctly pinned to the user's selected model.
        let params = ModelOptionsParams(
            explicitOnly: explicitOnly,
            sessionId: await currentSessionIdProvider()
        )
        let response = try await client.send(
            method: "model.options",
            params: params,
            timeoutNanos: 15_000_000_000
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
    ///
    /// The target string must match what the gateway's parse_model_switch_args
    /// accepts, and that depends on the provider type:
    ///   - openrouter (aggregator): model names are already vendor-qualified
    ///     ("deepseek/deepseek-v4-flash"). Prefixing the provider slug
    ///     produces "openrouter/deepseek/deepseek-v4-flash", which the
    ///     gateway cannot resolve ("Model not found in this provider's
    ///     listing"). Send the name as-is.
    ///   - Direct providers (anthropic, xiaomi): bare names. Send
    ///     "provider/name".
    ///   - Custom providers (custom:mbp-ollama): need the explicit
    ///     "--provider <slug>" form; "custom:mbp-ollama/qwen3:8b" and
    ///     "mbp-ollama/qwen3:8b" both fail resolution.
    func switchModel(_ name: String, provider: String) async throws {
        guard let client = await clientProvider() else {
            throw NativeGatewayClientError.notConnected
        }
        let target: String
        if provider.lowercased().hasPrefix("custom:") {
            target = "\(name) --provider \(provider)"
        } else if name.contains("/") {
            // Already vendor-qualified (aggregator form) - do NOT prefix.
            target = name
        } else {
            target = provider.isEmpty ? name : "\(provider)/\(name)"
        }
        var params: [String: String] = ["command": "/model \(target)"]
        // slash.exec requires a session_id (_sess_nowait 4001s without one).
        // Scope the switch to the active conversation when there is one so the
        // gateway can resolve the session instead of rejecting the command.
        if let sessionId = await currentSessionIdProvider() {
            params["session_id"] = sessionId
        }
        // LATENCY (build 37): slash.exec had no timeout override either,
        // inheriting the client's 60s default. The model picker's switch
        // path (selectModel -> ModelStore.switchModel -> slash.exec) would
        // spin its row spinner for the full 60s on a phantom socket before
        // surfacing an error — and since the gateway applies the switch
        // quickly on a healthy socket, the right bound is short enough to
        // fail fast but long enough for the slash worker to spawn on first
        // use (cold worker spawn + /model resolution can take a few
        // seconds). 30s bound.
        let response = try await client.send(
            method: "slash.exec",
            params: params,
            timeoutNanos: 30_000_000_000
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
        // LATENCY (build 37): cli.exec had no timeout override on the WS
        // send either (the `timeout` param only bounds the server-side
        // command execution). Same phantom-socket hang class as
        // model.options/slash.exec — bound it explicitly.
        // Give the client a bounded grace period beyond the gateway command
        // timeout. `hermes update --check` may legitimately spend most of its
        // server-side budget fetching origin; a fixed 60s socket deadline made
        // the Settings request fail before its advertised 120s command timeout.
        let serverTimeout = UInt64(max(1, min(timeout, 600)))
        let timeoutNanos = min((serverTimeout + 30) * 1_000_000_000, 630_000_000_000)
        let response = try await client.send(
            method: "cli.exec",
            params: CliExecParams(argv: argv, timeout: timeout),
            timeoutNanos: timeoutNanos
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

    /// Auxiliary task overrides from config. Mirrors the dashboard menu so
    /// every aux task the gateway can route is editable from the app.
    /// The list MUST stay in sync with the connector's `AUX_TASKS`
    /// and the Hermes gateway's `_AUX_TASK_SLOTS` source of truth.
    /// The wire format is decoded as a [String: TaskConfig] dictionary,
    /// so adding a new auxiliary.* key on the server only requires a
    /// corresponding entry in `auxiliaryCatalog` below.
    struct AuxTaskInfo {
        /// Canonical config key (snake_case), e.g. "skills_hub".
        /// Stable across reconnects and safe to use as a row identifier.
        var key: String
        /// Human label shown in the Settings menu.
        var label: String
        var provider: String
        var model: String

        var isOverride: Bool { provider != "auto" || !model.isEmpty }
    }

    /// Static catalog of auxiliary tasks. Order here is the order shown in
    /// the Settings menu and matches the connector AUX_TASKS so the app
    /// and the dashboard agree on which tasks exist. The canonical list
    /// is the Hermes gateway `_AUX_TASK_SLOTS` (web_server.py).
    static let auxiliaryCatalog: [(key: String, label: String)] = [
        ("vision",               "Vision"),
        ("web_extract",          "Web extract"),
        ("compression",          "Compression"),
        ("skills_hub",           "Skills hub"),
        ("approval",             "Approval"),
        ("mcp",                  "MCP"),
        ("title_generation",     "Title generation"),
        ("triage_specifier",     "Triage specifier"),
        ("kanban_decomposer",    "Kanban decomposer"),
        ("profile_describer",    "Profile describer"),
        ("curator",              "Curator"),
    ]

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

        // AuxiliarySection is decoded as a [String: TaskConfig] dictionary so
        // any auxiliary.* key the gateway returns flows through without a
        // code change. We then iterate the catalog above so menu order stays
        // deterministic and the label is owned client-side.
        let auxiliary = decoded.config?.auxiliary
        var rows: [AuxTaskInfo] = []
        for entry in Self.auxiliaryCatalog {
            let task = auxiliary?[entry.key]
            rows.append(AuxTaskInfo(
                key: entry.key,
                label: entry.label,
                provider: task?.provider ?? "auto",
                model: task?.model ?? ""
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
            // Generic dictionary keyed by the canonical auxiliary.* key
            // (e.g. "vision", "web_extract", "skills_hub"). The
            // previous version hard-coded three TaskConfig fields, which
            // silently dropped every task not in that whitelist from the
            // Settings menu even though the dashboard could configure
            // them.
            let tasks: [String: TaskConfig]

            init(from decoder: Decoder) throws {
                let container = try decoder.container(keyedBy: DynamicKey.self)
                var collected: [String: TaskConfig] = [:]
                for key in container.allKeys {
                    if let value = try? container.decode(TaskConfig.self, forKey: key) {
                        collected[key.stringValue] = value
                    }
                }
                self.tasks = collected
            }

            private struct DynamicKey: CodingKey {
                let stringValue: String
                var intValue: Int? { nil }
                init?(stringValue: String) { self.stringValue = stringValue }
                init?(intValue: Int) { return nil }
            }

            subscript(key: String) -> TaskConfig? { tasks[key] }
        }
        struct Config: Decodable {
            let auxiliary: AuxiliarySection?
        }
        let config: Config?
    }
}
