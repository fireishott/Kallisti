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
    private static let activeProfileKey = "kallisti.activeProfileName"

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?
    /// Native gateway feature client provider (nil when running legacy mode).
    /// Profiles have NO relay REST endpoint reachable in native mode, so the
    /// native path uses the gateway's cli.exec (`hermes profile list/use`).
    private let nativeFeatureClientProvider: @MainActor () -> NativeGatewayFeatureClient?

    init(
        apiClient: RelayAPIClient?,
        accessTokenProvider: @escaping () async -> String?,
        nativeFeatureClientProvider: @escaping @MainActor () -> NativeGatewayFeatureClient? = { nil }
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
        // Restore cached profile name immediately so ChatInputBar placeholder
        // reads correctly before the first network load completes.
        activeProfileName = UserDefaults.standard.string(forKey: Self.activeProfileKey)
    }

    /// The full profile object for the currently active profile, if any.
    var activeProfile: HeraldProfile? {
        guard let name = activeProfileName else { return nil }
        return profiles.first { $0.name == name }
    }

    /// The user-facing display name for the active profile.
    ///
    /// Sole "default" profile → "Ignyte" (known persona name for this
    /// consolidated single-profile deployment). Multiple profiles or
    /// non-default profiles retain their own name. The internal
    /// `activeProfileName` stays "default" for routing - only the display
    /// surface uses this.
    ///
    /// Before catalog load: `profiles` is empty, `activeProfileName` may
    /// be "default" from UserDefaults cache. This still resolves to "Ignyte"
    /// so the chat input bar and profile chip never flash "default".
    var displayProfileName: String {
        Self.resolveDisplayName(activeProfileName: activeProfileName, profileCount: profiles.count)
    }

    nonisolated static func resolveDisplayName(activeProfileName: String?, profileCount: Int) -> String {
        let name = activeProfileName ?? "default"
        return name == "default" && profileCount <= 1 ? "Ignyte" : name
    }

    func loadProfiles(force: Bool = false) async {
        if !force,
           let lastLoadedAt,
           Date().timeIntervalSince(lastLoadedAt) < Self.refreshInterval,
           !profiles.isEmpty {
            return
        }

        // NATIVE path: cli.exec `hermes profile list`. The output is a table:
        //   Profile   Model   Gateway   Alias   Distribution
        //   default   ...     stopped   -       -
        //   ◆ignyte    ...     running   -       -
        // Active profile is prefixed with ◆. Parsing is column-based: take
        // the first whitespace-delimited field, strip the ◆ marker.
        if let featureClient = nativeFeatureClientProvider() {
            isLoading = true
            errorMessage = nil
            defer { isLoading = false }
            do {
                let output = try await featureClient.cliExec(argv: ["profile", "list"])
                var parsed: [HeraldProfile] = []
                var activeName: String?
                for rawLine in output.components(separatedBy: .newlines) {
                    let line = rawLine.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !line.isEmpty, !line.hasPrefix("Profile"),
                          !line.hasPrefix("─"), !line.hasPrefix("-"),
                          !line.hasPrefix("-")
                    else { continue }
                    let isActive = line.hasPrefix("◆")
                    let nameField = line
                        .replacingOccurrences(of: "◆", with: "")
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                    let name = nameField.split(separator: " ", maxSplits: 1).first.map(String.init) ?? nameField
                    guard !name.isEmpty else { continue }
                    parsed.append(HeraldProfile(name: name, description: "", skillCount: 0))
                    if isActive { activeName = name }
                }
                if !parsed.isEmpty {
                    // Build 97: the CLI table has no skill counts, so the Hub
                    // showed "0 skills" for every profile.  The connector's
                    // /v1/profiles facade endpoint returns real per-profile
                    // skill counts - merge them in, keeping CLI names as the
                    // source of truth for which profiles exist (facade can be
                    // down while the gateway WS is up).
                    if let catalog = await featureClient.profileCatalog() {
                        let counts = Dictionary(
                            catalog.profiles.map { ($0.name, $0.skillCount) },
                            uniquingKeysWith: { first, _ in first }
                        )
                        parsed = parsed.map { profile in
                            HeraldProfile(
                                name: profile.name,
                                description: profile.description,
                                skillCount: counts[profile.name] ?? profile.skillCount
                            )
                        }
                        if catalog.active != nil {
                            activeName = catalog.active
                        }
                    }
                    profiles = parsed
                    if let activeName {
                        activeProfileName = activeName
                        UserDefaults.standard.set(activeName, forKey: Self.activeProfileKey)
                    }
                    lastLoadedAt = .now
                    return
                }
                // Fall through to legacy path on empty parse.
            } catch {
                errorMessage = nil
                // Fall through to legacy path.
            }
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
            // Only replace profiles after new data arrives - never set
            // profiles = [] as an intermediate step, which would cause the
            // profile/model chips to vanish mid-session.
            profiles = response.profiles
            // Only update activeProfileName if the server actually returns one.
            // A nil response means the connector doesn't track active profiles -
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
    ///
    /// Primary path: ``POST /v1/profile`` - the relay forwards to the
    /// connector's ``profile.set`` RPC, which writes config.yaml and
    /// restarts the Hermes gateway so the new profile takes effect.
    ///
    /// Fallback: ``POST /gw/profile/switch`` - gateway control plane,
    /// used when the connector RPC path is unavailable.
    func switchProfile(to name: String) async throws {
        // NATIVE path: `hermes profile use <name>` over cli.exec. This is the
        // same CLI the connector's profile.set RPC wraps; it writes the
        // sticky default profile so the next gateway start uses it.
        if let featureClient = nativeFeatureClientProvider() {
            // Optimistic local update before the network call
            markActive(name)
            do {
                _ = try await featureClient.cliExec(argv: ["profile", "use", name])
                await loadProfiles(force: true)
                return
            } catch {
                errorMessage = error.localizedDescription
                throw error
            }
        }

        guard let apiClient, let token = await accessTokenProvider() else {
            errorMessage = "Not connected to a relay."
            return
        }

        // Optimistic local update before the network call
        markActive(name)

        do {
            // Primary path: connector RPC via relay
            let body = ["name": name]
            let _: ProfileSetResponse = try await apiClient.post(
                path: "profile",
                body: body,
                accessToken: token
            )
        } catch {
            // Fallback: gateway control plane
            let gwBody: [String: String] = ["profile": name]
            let _: GatewayResponse = try await apiClient.postGateway(
                path: "gw/profile/switch", body: gwBody, accessToken: token
            )
        }

        // Confirm by reloading the catalog from the server
        await loadProfiles(force: true)
    }

    private struct GatewayResponse: Decodable {
        let switched: Bool?
        let profile: String?
    }

    /// Optimistically marks a profile active - used when the chat path
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
