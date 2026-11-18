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

    /// The daemon, found through the container when the store was built before it.
    private var resolvedHerald: (any HeraldClientProtocol)? {
        // `if let herald` reads the stored property. The earlier spelling
        // (`if let herald = resolvedHerald`) called this getter from inside
        // itself, which is a stack overflow the first time anything asked for
        // it - exactly what build 204 did on Skills and Cron.
        if let herald { return herald }
        return AppContainer.shared?.chatStore.heraldClient
    }

    /// Give the store the daemon once the container has built one. The stores are
    /// constructed before the chat store, which is what owns the client.
    func attach(herald: any HeraldClientProtocol) {
        self.herald = herald
    }

    /// The daemon, when one is paired. Without it the catalogue is always empty:
    /// the relay this store was written for is gone.
    private(set) var herald: (any HeraldClientProtocol)?

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?

    init(
        apiClient: RelayAPIClient?,
        accessTokenProvider: @escaping () async -> String?,
        herald: (any HeraldClientProtocol)? = nil
    ) {
        self.herald = herald
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
        // A paired daemon is the catalogue. The relay branch stays for a build that
        // still carries one; an Eris phone must never read "no skills installed"
        // when the truth is that its token was minted without administration - the
        // daemon says exactly that and it belongs on screen.
        if let herald = resolvedHerald {
            if !force, let lastLoadedAt, Date().timeIntervalSince(lastLoadedAt) < Self.refreshInterval {
                return
            }
            isLoading = true
            errorMessage = nil
            defer { isLoading = false }
            do {
                let rows = try await herald.skillCatalog()
                skills = rows.map { row in
                    HeraldSkill(
                        name: row.name,
                        description: row.description,
                        path: row.path ?? row.key
                    )
                }
                lastLoadedAt = .now
            } catch {
                errorMessage = (error as? ErisError)?.message ?? error.localizedDescription
            }
            return
        }
        await loadSkillsFromRelay(force: force)
    }

    private func loadSkillsFromRelay(force: Bool) async {
    }

    func reset() {
        skills = []
        errorMessage = nil
        lastLoadedAt = nil
        searchText = ""
    }
}
