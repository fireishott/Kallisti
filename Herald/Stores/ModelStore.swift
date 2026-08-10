import Foundation
import OSLog

/// Loads the model catalog from the connected Hermes host.
///
/// NATIVE path (preferred): `model.options` over the gateway WebSocket -
/// the authoritative server-side catalog. Switching goes through
/// `slash.exec` `/model <provider>/<name>`.
///
/// LEGACY path (fallback): `GET /v1/models` + `POST /v1/model` via the relay
/// connector. Kept for pre-native compatibility; the connector REST facade
/// is dead for native clients (native bearer tokens are rejected on
/// `/v1/*` and `/gw/*`).
@MainActor
@Observable
final class ModelStore {
    struct HeraldModel: Decodable, Identifiable, Hashable {
        let name: String
        let provider: String
        let providerName: String?
        let contextWindow: Int?
        let isProviderDefault: Bool?

        var id: String { "\(provider)/\(name)" }
        var displayProviderName: String { providerName ?? provider }
    }

    struct ActiveModel: Decodable, Hashable {
        let name: String
        let provider: String?
        let contextWindow: Int?
    }

    private struct ModelCatalogResponse: Decodable {
        let models: [HeraldModel]?
        let activeModel: ActiveModel?
    }

    private struct ModelSetResponse: Decodable {
        let activeModel: ActiveModel?
    }

    private(set) var models: [HeraldModel] = []
    /// Generation token bumped on every switchModel call. Any loadModels
    /// response that was in flight when a switch started MUST NOT overwrite
    /// the optimistic write.
    private var switchGeneration: UInt64 = 0
    private(set) var activeModel: ActiveModel?
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    var isError: Bool { errorMessage != nil }
    var currentModel: ActiveModel? { activeModel }
    private var lastLoadedAt: Date?

    private static let refreshInterval: TimeInterval = 60

    /// Build 55: number of self-healing retries after a native catalog load
    /// fails while we hold no cached catalog (e.g. first load racing the
    /// gateway socket during reconnect churn). Capped so a dead gateway
    /// doesn't spin forever; the retry button and the .connected reload
    /// handler remain the durable recovery paths.
    private static let maxNativeRetries = 5
    private var nativeRetryCount = 0

    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "ModelStore")

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?
    /// Native gateway feature client provider (nil when running legacy mode).
    private let nativeFeatureClientProvider: @MainActor () -> NativeGatewayFeatureClient?

    init(
        apiClient: RelayAPIClient?,
        accessTokenProvider: @escaping () async -> String?,
        nativeFeatureClientProvider: @escaping @MainActor () -> NativeGatewayFeatureClient? = { nil }
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
    }

    /// Models grouped by provider display name, providers sorted alphabetically
    /// with the active model's provider first.
    var modelsByProvider: [(provider: String, models: [HeraldModel])] {
        let grouped = Dictionary(grouping: models, by: \.displayProviderName)
        return grouped
            .map { (provider: $0.key, models: $0.value) }
            .sorted { lhs, rhs in
                let activeProvider = models.first { $0.name == activeModel?.name }?.displayProviderName
                if lhs.provider == activeProvider { return true }
                if rhs.provider == activeProvider { return false }
                return lhs.provider.localizedCaseInsensitiveCompare(rhs.provider) == .orderedAscending
            }
    }

    func isActive(_ model: HeraldModel) -> Bool {
        model.name == activeModel?.name
    }

    func loadModels(force: Bool = false) async {
        if !force,
           let lastLoadedAt,
           Date().timeIntervalSince(lastLoadedAt) < Self.refreshInterval,
           !models.isEmpty {
            return
        }

        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        // NATIVE path: model.options over the gateway socket.
        if let featureClient = nativeFeatureClientProvider() {
            do {
                let info = try await featureClient.modelOptions(explicitOnly: false)
                // The server emits a `custom:<name>` row for every keyed
                // provider (providers_dict_to_custom_providers). Those rows
                // carry no API key and resolve to empty ProviderDefs, so
                // switching to them 401s and the gateway silently falls back
                // to a different model. Dedupe: a model shown by a keyed
                // provider wins over its custom:* twin; models only available
                // via custom:* (e.g. mbp-ollama qwen3:8b) stay.
                let allRows = info.providers
                let keyedNames = Set(
                    allRows
                        .filter { !$0.slug.hasPrefix("custom:") }
                        .flatMap { $0.modelNames }
                        .map { $0.lowercased() }
                )
                let dedupedRows = allRows.filter { provider in
                    if !provider.slug.hasPrefix("custom:") { return true }
                    return !(provider.modelNames.contains {
                        keyedNames.contains($0.lowercased())
                    })
                }
                models = dedupedRows.flatMap { provider in
                    provider.modelNames.map { name in
                        HeraldModel(
                            name: name,
                            provider: provider.slug,
                            providerName: provider.displayName,
                            contextWindow: nil,
                            isProviderDefault: nil
                        )
                    }
                }
                // Build 60: capture generation BEFORE touching activeModel. If a switch
                // started while we were awaiting modelOptions, drop the activeModel
                // write to avoid clobbering the optimistic switch result.
                let genAtEntry = switchGeneration
                if let model = info.model, genAtEntry == switchGeneration {
                    activeModel = ActiveModel(
                        name: model,
                        provider: info.provider,
                        contextWindow: nil
                    )
                }
                lastLoadedAt = .now
                nativeRetryCount = 0
                return
            } catch {
                // Build 41: DO NOT fall through to the legacy relay path.
                // GET /v1/models over the relay is dead for native clients
                // (native bearer tokens are rejected on /v1/*) and returns
                // only the API-server's two placeholder rows (Ignyte,
                // hermes-agent) - no real catalog, and definitely no 9router.
                // Falling back made retry hit the same dead path forever and
                // hid the actual native error. Surface the real error so the
                // picker's retry button re-runs loadModels(force: true) and
                // retries the NATIVE path (which heals via reconnect).
                // Match on the error's localized description so this file
                // doesn't need to reference the transport error enum directly.
                let desc = (error as NSError).localizedDescription.lowercased()
                let message: String
                if desc.contains("timed out") || desc.contains("timeout") {
                    message = "Gateway timed out. Check the connection and retry."
                } else if desc.contains("transport") || desc.contains("connection dropped") || desc.contains("closed") {
                    message = "Gateway connection dropped. Retrying reconnects."
                } else if desc.contains("not connected") {
                    message = "Not connected to the gateway."
                } else {
                    message = error.localizedDescription
                }
                errorMessage = message
                // Build 55: self-healing retry. On the iPad the first
                // loadModels often runs while the gateway socket is still
                // coming up (reconnect churn), errors silently, and the pill
                // stays a bare dot until the user opens the picker. When the
                // native load fails and we have no cached catalog, schedule a
                // few short-delay retries instead of waiting for a manual
                // action. Each attempt re-enters the NATIVE path, which heals
                // once the socket is actually live.
                if models.isEmpty, nativeRetryCount < Self.maxNativeRetries {
                    nativeRetryCount += 1
                    let attempt = nativeRetryCount
                    Task { @MainActor in
                        try? await Task.sleep(for: .seconds(3))
                        guard !Task.isCancelled, self.models.isEmpty else { return }
                        Self.logger.info("ModelStore retry \\(attempt)/\\(Self.maxNativeRetries) after native load failure")
                        await self.loadModels(force: true)
                    }
                }
                return
            }
        }

        // Legacy relay path - only used when no native feature client exists
        // (e.g. pre-auth onboarding). NOT a fallback for native failures.
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }

        do {
            let response: ModelCatalogResponse = try await apiClient.get(
                path: "models",
                accessToken: token
            )
            models = response.models ?? []
            // Build 28: a nil activeModel in the response does not mean
            // "no model configured" — the relay returns {models: [],
            // activeModel: null} when the connector RPC fails, which is
            // indistinguishable from an unconfigured host.  Only replace
            // a previously confirmed active model when the server
            // explicitly names one.
            if let serverActive = response.activeModel {
                activeModel = serverActive
            }
            // Build 33: never synthesise activeModel from models.first.
            // The first catalog entry may not be the actual active model,
            // and a nil activeModel correctly surfaces "Model unavailable"
            // rather than silently showing the wrong model.
            // Only an explicit authoritative unconfigured state from the
            // server may clear a previously confirmed active model.
            lastLoadedAt = .now
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Switches the active model.
    ///
    /// NATIVE path: `slash.exec` `/model <provider>/<name>` on the gateway
    /// socket (uses the gateway's own model-switch machinery - session-scoped
    /// or persistent per the gateway's default scope).
    ///
    /// LEGACY path: `POST /v1/model` via relay, falling back to
    /// `POST /gw/model/switch`.
    func switchModel(to name: String, provider: String) async throws {
        if let featureClient = nativeFeatureClientProvider() {
            do {
                // Build 60: bump generation BEFORE slash.exec so any in-flight
                // loadModels is invalidated against the optimistic write.
                switchGeneration &+= 1
                try await featureClient.switchModel(name, provider: provider)
                // Optimistic: slash.exec returned success, gateway applied
                // the switch. Do NOT call loadModels here -- it re-enters
                // currentSessionIdProvider -> ensureSessionForSwitch which
                // can hit a phantom socket and recreate the session, after
                // which model.options returns the session default, clobbering
                // this write. The next natural loadModels call will refresh
                // against the now-correct session scope.
                activeModel = ActiveModel(name: name, provider: provider, contextWindow: nil)
                return
            } catch {
                // NATIVE mode: do NOT fall through to the legacy relay path.
                // The native bearer token is only valid on the gateway
                // (dashboard port 9119); the relay facade rejects it, so the
                // legacy POST always surfaces a confusing "relay error" that
                // hides the real cause. Surface the actual native error.
                throw error
            }
        }

        guard let apiClient, let token = await accessTokenProvider() else {
            throw ModelStoreError.notConnected
        }

        do {
            // Primary path: connector RPC via relay
            let body = ["name": name, "provider": provider]
            let response: ModelSetResponse = try await apiClient.post(
                path: "model", body: body, accessToken: token
            )
            if let updated = response.activeModel {
                activeModel = updated
            }
        } catch {
            // Fallback: gateway control plane (model name only)
            let gwBody: [String: String] = ["model": name]
            let _: GatewayResponse = try await apiClient.postGateway(
                path: "gw/model/switch", body: gwBody, accessToken: token
            )
            // Optimistic — reload catalog to confirm
            await loadModels(force: true)
        }
    }

    private struct GatewayResponse: Decodable {
        let switched: Bool?
        let model: String?
    }

    enum ModelStoreError: Error, LocalizedError {
        case notConnected

        var errorDescription: String? {
            switch self {
            case .notConnected: "Not connected to a relay."
            }
        }
    }

    func reset() {
        models = []
        activeModel = nil
        errorMessage = nil
        lastLoadedAt = nil
    }
}
