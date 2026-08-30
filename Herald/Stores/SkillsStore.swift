import Foundation

/// Loads the skill catalog from the connected Hermes host via the relay.
///
/// Listing comes from `GET /skills` (the skill tree on the host).
/// This store is read-only — skills are defined on the host and surfaced here
/// for browsing.
@MainActor
@Observable
final class SkillsStore {
    struct HeraldSkill: Decodable, Identifiable, Hashable {
        let name: String
        let description: String
        let path: String

        var id: String { name }
    }

    private struct SkillCatalogResponse: Decodable {
        let skills: [HeraldSkill]
    }

    private(set) var skills: [HeraldSkill] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?
    var searchText = ""

    private var lastLoadedAt: Date?

    private static let refreshInterval: TimeInterval = 120

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?
    private let nativeFeatureClientProvider: () -> NativeGatewayFeatureClient?

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?, nativeFeatureClientProvider: @escaping () -> NativeGatewayFeatureClient? = { nil }) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
    }

    /// Skills filtered by the current search text, matching on name or description.
    var filteredSkills: [HeraldSkill] {
        if searchText.isEmpty { return skills }
        return skills.filter {
            $0.name.localizedCaseInsensitiveContains(searchText) ||
            $0.description.localizedCaseInsensitiveContains(searchText)
        }
    }

    func loadSkills(force: Bool = false) async {
        if !force,
           let lastLoadedAt,
           Date().timeIntervalSince(lastLoadedAt) < Self.refreshInterval,
           !skills.isEmpty {
            return
        }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            if let nativeFeatureClient = nativeFeatureClientProvider() {
                skills = try await nativeFeatureClient.managedSkills().map { HeraldSkill(name: $0.name, description: $0.description, path: $0.path) }
            } else {
                guard let apiClient, let token = await accessTokenProvider() else {
                    // Build 135.40: keep whatever we already have on a
                    // connectivity miss instead of nuking the list to empty.
                    errorMessage = "Not connected to a relay."
                    return
                }
                let response: SkillCatalogResponse = try await apiClient.get(path: "skills", accessToken: token)
                skills = response.skills
            }
            lastLoadedAt = .now
        } catch {
            // Build 135.40: a failed refresh must NOT clear the loaded list.
            // The old code left `skills` as-is but the view had no error UI,
            // so a transient RPC failure during back-nav rendered the bare
            // yellow-triangle empty state (and a successful list vanished).
            // Surface the error for the view; the stale list stays visible.
            if skills.isEmpty {
                errorMessage = error.localizedDescription
            }
        }
    }

    func reset() {
        skills = []
        errorMessage = nil
        lastLoadedAt = nil
        searchText = ""
    }
}
