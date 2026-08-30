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

    /// Build 135.41: full skill detail (SKILL.md content) from the
    /// connector's /v1/skills/{name} endpoint.
    struct SkillDetail: Decodable, Sendable {
        let name: String
        let path: String?
        let description: String?
        let content: String?
    }

    private struct SkillDetailEnvelope: Decodable {
        let data: SkillDetail
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
            // Build 135.41: prefer the REST /v1/skills catalog (real name +
            // description + path from the connector) over the native gateway
            // path. The gateway's skills.manage(action:"list") returns only
            // {category: [names]} — no descriptions, no paths — which made the
            // iOS Skills browser show category names as descriptions and
            // empty detail shells.
            if let apiClient, let token = await accessTokenProvider() {
                let response: SkillCatalogResponse = try await apiClient.get(path: "skills", accessToken: token)
                skills = response.skills
            } else if let nativeFeatureClient = nativeFeatureClientProvider() {
                skills = try await nativeFeatureClient.managedSkills().map { HeraldSkill(name: $0.name, description: $0.description, path: $0.path) }
            } else {
                errorMessage = "Not connected to a relay."
                return
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

    /// Build 135.41: fetch one skill's SKILL.md content via the REST
    /// /v1/skills/{name} endpoint (falls back to native gateway if REST is
    /// unavailable). Returns the parsed detail or throws.
    func loadDetail(name: String) async throws -> SkillDetail {
        if let apiClient, let token = await accessTokenProvider() {
            do {
                let envelope: SkillDetailEnvelope = try await apiClient.get(path: "skills/\(name)", accessToken: token)
                return envelope.data
            } catch {
                // Fall through to native gateway on REST failure
            }
        }
        if let nativeFeatureClient = nativeFeatureClientProvider() {
            // Native gateway has no per-skill content endpoint; return a
            // minimal detail from the list entry so the view still renders.
            if let skill = skills.first(where: { $0.name == name }) {
                return SkillDetail(name: skill.name, path: skill.path, description: skill.description, content: nil)
            }
        }
        throw URLError(.badURL)
    }

    func reset() {
        skills = []
        errorMessage = nil
        lastLoadedAt = nil
        searchText = ""
    }
}
