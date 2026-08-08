import Foundation

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
    private(set) var activeModel: ActiveModel?
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    var isError: Bool { errorMessage != nil }
    var currentModel: ActiveModel? { activeModel }
    private var lastLoadedAt: Date?

    private static let refreshInterval: TimeInterval = 60

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
                models = info.providers.flatMap { provider in
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
                if let model = info.model {
                    activeModel = ActiveModel(
                        name: model,
                        provider: info.provider,
                        contextWindow: nil
                    )
                }
                lastLoadedAt = .now
                return
            } catch {
                // Fall through to legacy path rather than surfacing an error:
                // the catalog is still reachable via the relay when native
                // model.options is unavailable.
                errorMessage = nil
            }
        }

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
                try await featureClient.switchModel(name, provider: provider)
                // Optimistic — reload the catalog to confirm the switch.
                activeModel = ActiveModel(name: name, provider: provider, contextWindow: nil)
                await loadModels(force: true)
                return
            } catch {
                // Native switch failed; fall through to legacy path.
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
