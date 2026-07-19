import Foundation

/// Loads the model catalog from the connected Hermes host via the relay.
///
/// Listing comes from `GET /v1/models` (config.yaml providers on the host).
/// Switching goes through `POST /v1/model`, which edits the host's
/// config.yaml directly and returns the resulting active model — that
/// response is the source of truth for `activeModel`, not an optimistic guess.
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
    private var lastLoadedAt: Date?

    private static let refreshInterval: TimeInterval = 60

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
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
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }

        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            let response: ModelCatalogResponse = try await apiClient.get(
                path: "models",
                accessToken: token
            )
            models = response.models ?? []
            activeModel = response.activeModel
            lastLoadedAt = .now
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Switches the active model via `POST /v1/model`. The relay edits the
    /// host's config.yaml and returns the resulting active model, which is
    /// applied directly here — no optimistic guessing.
    func switchModel(to name: String, provider: String) async throws {
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }
        let body = ["name": name, "provider": provider]
        let response: ModelSetResponse = try await apiClient.post(
            path: "model", body: body, accessToken: token
        )
        if let updated = response.activeModel {
            activeModel = updated
        }
    }

    func reset() {
        models = []
        activeModel = nil
        errorMessage = nil
        lastLoadedAt = nil
    }
}
