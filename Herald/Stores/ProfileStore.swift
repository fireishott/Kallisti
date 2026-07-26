import Foundation

/// Loads the profile catalog from the connected Hermes host via the relay.
///
/// Profile listing comes from `GET /v1/profiles`. Active-profile switching
/// uses `POST /v1/profile` to set the profile on the host, then reloads the
/// catalog to confirm.
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

    private struct ProfileSetResponse: Decodable {
        let activeProfile: String?
    }

    var profiles: [HeraldProfile] = []
    private(set) var activeProfileName: String?
    private(set) var isLoading = false
    private(set) var errorMessage: String?
    private var lastLoadedAt: Date?

    private static let refreshInterval: TimeInterval = 60
    private static let activeProfileKey = "herald.activeProfileName"

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        // Restore cached profile name immediately so ChatInputBar placeholder
        // reads correctly before the first network load completes.
        activeProfileName = UserDefaults.standard.string(forKey: Self.activeProfileKey)
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
            // Only update activeProfileName if the server actually returns one.
            // A nil response means the connector doesn't track active profiles —
            // preserve our local state so the chip doesn't vanish.
            if let name = response.activeProfile?.name {
                activeProfileName = name
                UserDefaults.standard.set(name, forKey: Self.activeProfileKey)
            }
            lastLoadedAt = .now
        } catch {
            // Keep existing profiles on transient errors so chips don't vanish.
            errorMessage = error.localizedDescription
        }
    }

    /// Optimistically marks a profile active and calls the relay endpoint
    /// to persist the selection on the host.
    func switchProfile(to name: String) async throws {
        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }

        // Optimistic local update before the network call
        markActive(name)

        let body = ["name": name]
        let _: ProfileSetResponse = try await apiClient.post(
            path: "profile",
            body: body,
            accessToken: token
        )

        // Confirm by reloading the catalog from the server
        await loadProfiles(force: true)
    }

    /// Optimistically marks a profile active — used when the chat path
    /// detects a profile switch in the agent's response text.
    func markActive(_ name: String) {
        activeProfileName = name
        UserDefaults.standard.set(name, forKey: Self.activeProfileKey)
    }

    func reset() {
        profiles = []
        activeProfileName = nil
        errorMessage = nil
        lastLoadedAt = nil
        UserDefaults.standard.removeObject(forKey: Self.activeProfileKey)
    }
}
