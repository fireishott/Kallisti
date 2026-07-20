import Foundation

/// Loads the profile catalog from the connected Hermes host via the relay.
///
/// Listing comes from `GET /profiles` (the profile tree on the host).
/// Active-profile switching is dispatched through the chat path, so this store
/// is read-only apart from an optimistic active-profile update while the
/// gateway confirmation streams back.
@MainActor
@Observable
final class ProfileStore {
    struct HeraldProfile: Decodable, Identifiable, Hashable {
        let name: String
        let description: String
        let skillCount: Int

        var id: String { name }
    }

    private struct ProfileCatalogResponse: Decodable {
        let activeProfile: HeraldProfile?
        let profiles: [HeraldProfile]
    }

    private(set) var profiles: [HeraldProfile] = []
    private(set) var activeProfileName: String?
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

    /// The full profile object for the currently active profile, if any.
    var activeProfile: HeraldProfile? {
        guard let name = activeProfileName else { return nil }
        return profiles.first { $0.name == name }
    }

    func loadProfiles(force: Bool = false) async {
        if !force,
           let lastLoadedAt,
           Date().timeIntervalSince(lastLoadedAt) < Self.refreshInterval,
           !profiles.isEmpty {
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
            let response: ProfileCatalogResponse = try await apiClient.get(
                path: "profiles",
                accessToken: token
            )
            // Only replace profiles after new data arrives — never set
            // profiles = [] as an intermediate step, which would cause the
            // profile/model chips to vanish mid-session.
            profiles = response.profiles
            activeProfileName = response.activeProfile?.name
            lastLoadedAt = .now
        } catch {
            // Keep existing profiles on transient errors so chips don't vanish.
            errorMessage = error.localizedDescription
        }
    }

    /// Optimistically marks a profile active while the profile-switch command's
    /// confirmation streams back through the chat path.
    func markActive(_ name: String) {
        activeProfileName = name
    }

    func reset() {
        profiles = []
        activeProfileName = nil
        errorMessage = nil
        lastLoadedAt = nil
    }
}
