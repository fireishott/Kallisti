import Foundation

/// Loads the skill catalog from the connected Herald host via the relay.
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

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
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
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }

        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            let response: SkillCatalogResponse = try await apiClient.get(
                path: "skills",
                accessToken: token
            )
            skills = response.skills
            lastLoadedAt = .now
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func reset() {
        skills = []
        errorMessage = nil
        lastLoadedAt = nil
        searchText = ""
    }
}
