import Foundation

enum PushTransportMode: String, Codable, Hashable, Sendable {
    case direct
    case relay
}

struct AppBuildConfiguration: Equatable, Sendable {
    let hostedRelayBaseURL: String?
    let hostedRelayEnabled: Bool
    let pushTransport: PushTransportMode
    let pushBrokerBaseURL: URL?
    let supportURL: URL?
    let termsOfServiceURL: URL?
    let privacyPolicyURL: URL?

    static func current(bundle: Bundle = .main) -> AppBuildConfiguration {
        AppBuildConfiguration(infoDictionary: bundle.infoDictionary ?? [:])
    }

    init(infoDictionary: [String: Any]) {
        let info = infoDictionary
        let hostedRelayBaseURL = RelayConfiguration.normalizeBaseURL(
            info["APP_HOSTED_RELAY_URL"] as? String
        )
        let hostedRelayEnabled = (info["APP_HOSTED_RELAY_ENABLED"] as? Bool) ?? false
        let pushTransport = PushTransportMode(
            rawValue: ((info["APP_PUSH_TRANSPORT"] as? String) ?? "direct")
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .lowercased()
        ) ?? .direct

        let pushBrokerBaseURL: URL? = {
            guard let raw = info["APP_PUSH_BROKER_URL"] as? String else { return nil }
            guard let normalized = RelayConfiguration.normalizeBaseURL(raw) else { return nil }
            return URL(string: normalized)
        }()

        func urlValue(_ key: String) -> URL? {
            guard let raw = info[key] as? String, !raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                return nil
            }
            return URL(string: raw)
        }

        self.hostedRelayBaseURL = hostedRelayBaseURL
        self.hostedRelayEnabled = hostedRelayEnabled && hostedRelayBaseURL != nil
        self.pushTransport = pushTransport
        self.pushBrokerBaseURL = pushBrokerBaseURL
        self.supportURL = urlValue("APP_SUPPORT_URL")
        self.termsOfServiceURL = urlValue("APP_TERMS_URL")
        self.privacyPolicyURL = urlValue("APP_PRIVACY_URL")
    }

    var usesManagedPushBroker: Bool {
        pushTransport == .relay && pushBrokerBaseURL != nil
    }
}

enum RelayMode: String, Codable, CaseIterable, Hashable, Sendable {
    case custom
    case hosted

    var displayLabel: String {
        switch self {
        case .custom: "Self-Hosted"
        case .hosted: "Managed"
        }
    }
}

enum RelayConnectionMode: String, Codable, CaseIterable, Hashable, Sendable {
    case tailscale
    case selfHostedRelay

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let rawValue = try container.decode(String.self)
        switch rawValue {
        case "tailscale":
            self = .tailscale
        case "managedRelay", "selfHostedRelay":
            self = .selfHostedRelay
        default:
            self = .selfHostedRelay
        }
    }

    init(legacyRelayMode: RelayMode) {
        switch legacyRelayMode {
        case .hosted, .custom:
            self = .selfHostedRelay
        }
    }

    var legacyRelayMode: RelayMode {
        .custom
    }

    var displayLabel: String {
        switch self {
        case .tailscale:
            return "Tailscale"
        case .selfHostedRelay:
            return "Self-Hosted Relay"
        }
    }

    var compactLabel: String {
        switch self {
        case .tailscale:
            return "Tailscale"
        case .selfHostedRelay:
            return "Relay URL"
        }
    }

    var shortDescription: String {
        switch self {
        case .tailscale:
            return "Private tailnet reachability for a local Hermes relay."
        case .selfHostedRelay:
            return "Bring your own public Hermes relay URL."
        }
    }

    var usesCustomRelayURL: Bool {
        true
    }

    var reliesOnOfficialPushRelay: Bool {
        false
    }

    var defaultOfflineMessage: String {
        switch self {
        case .tailscale:
            return "Open Tailscale or reconnect to your tailnet to reach Herald."
        case .selfHostedRelay:
            return "Your self-hosted relay URL is not reachable."
        }
    }

    var hostOfflineMessage: String {
        switch self {
        case .tailscale:
            return "The relay is reachable, but the connector is offline. Keep the Mac relay running to queue messages."
        case .selfHostedRelay:
            return "Your relay is reachable, but the connector is offline. Messages can queue while it stays online."
        }
    }

    var notConnectedMessage: String {
        switch self {
        case .tailscale:
            return "Pair a Hermes host on your tailnet before sending messages."
        case .selfHostedRelay:
            return "Pair a Hermes host with this self-hosted relay before sending messages."
        }
    }

    var unreachableSendBlockedMessage: String {
        switch self {
        case .tailscale:
            return "Can't reach your tailnet relay. Open Tailscale to reconnect, then send again."
        case .selfHostedRelay:
            return "Your self-hosted relay URL is not reachable. Check the URL in Settings and try again."
        }
    }

    var unreachableActionLabel: String {
        switch self {
        case .selfHostedRelay:
            return "Retry"
        case .tailscale:
            return "Open Tailscale"
        }
    }

    var unreachableActionDeepLink: URL? {
        switch self {
        case .tailscale:
            return URL(string: "tailscale://")
        case .selfHostedRelay:
            return nil
        }
    }

    var relayURLHint: String? {
        switch self {
        case .tailscale:
            return "Use your tailnet URL — e.g. https://my-mac.tail-scale.ts.net/v1 — or run `tailscale serve` to proxy a local relay."
        case .selfHostedRelay:
            return "Point this at your public Hermes relay — e.g. https://relay.example.com/v1."
        }
    }

    var backgroundDeliveryNote: String {
        switch self {
        case .tailscale:
            return "Tailscale mode stays honest: messages arrive while the app is in the foreground or reconnected on your tailnet. No official background push."
        case .selfHostedRelay:
            return "Self-hosted relays don't receive official push credentials. Background delivery depends on your relay's own notification channel."
        }
    }
}

struct RelayConfiguration: Codable, Hashable, Sendable {
    var relayMode: RelayMode
    var connectionMode: RelayConnectionMode
    var customRelayBaseURL: String
    var hostedRelayBaseURL: String?
    var hostedRelayEnabled: Bool

    init(
        relayMode: RelayMode = .custom,
        customRelayBaseURL: String = "",
        hostedRelayBaseURL: String? = nil,
        hostedRelayEnabled: Bool = false
    ) {
        let resolvedConnectionMode = RelayConnectionMode(legacyRelayMode: relayMode)
        self.relayMode = resolvedConnectionMode.legacyRelayMode
        self.connectionMode = resolvedConnectionMode
        self.customRelayBaseURL = RelayConfiguration.normalizeBaseURL(customRelayBaseURL) ?? customRelayBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        self.hostedRelayBaseURL = RelayConfiguration.normalizeBaseURL(hostedRelayBaseURL)
        self.hostedRelayEnabled = hostedRelayEnabled && self.hostedRelayBaseURL != nil
    }

    init(
        connectionMode: RelayConnectionMode,
        customRelayBaseURL: String = "",
        hostedRelayBaseURL: String? = nil,
        hostedRelayEnabled: Bool = false
    ) {
        self.relayMode = connectionMode.legacyRelayMode
        self.connectionMode = connectionMode
        self.customRelayBaseURL = RelayConfiguration.normalizeBaseURL(customRelayBaseURL) ?? customRelayBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        self.hostedRelayBaseURL = RelayConfiguration.normalizeBaseURL(hostedRelayBaseURL)
        self.hostedRelayEnabled = hostedRelayEnabled && self.hostedRelayBaseURL != nil
    }

    private enum CodingKeys: String, CodingKey {
        case relayMode
        case connectionMode
        case customRelayBaseURL
        case hostedRelayBaseURL
        case hostedRelayEnabled
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let decodedRelayMode = try container.decodeIfPresent(RelayMode.self, forKey: .relayMode) ?? .custom
        let decodedConnectionMode = try container.decodeIfPresent(RelayConnectionMode.self, forKey: .connectionMode)
            ?? RelayConnectionMode(legacyRelayMode: decodedRelayMode)
        self.init(
            connectionMode: decodedConnectionMode,
            customRelayBaseURL: try container.decodeIfPresent(String.self, forKey: .customRelayBaseURL) ?? "",
            hostedRelayBaseURL: try container.decodeIfPresent(String.self, forKey: .hostedRelayBaseURL),
            hostedRelayEnabled: try container.decodeIfPresent(Bool.self, forKey: .hostedRelayEnabled) ?? false
        )
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(relayMode, forKey: .relayMode)
        try container.encode(connectionMode, forKey: .connectionMode)
        try container.encode(customRelayBaseURL, forKey: .customRelayBaseURL)
        try container.encodeIfPresent(hostedRelayBaseURL, forKey: .hostedRelayBaseURL)
        try container.encode(hostedRelayEnabled, forKey: .hostedRelayEnabled)
    }

    mutating func updateConnectionMode(_ mode: RelayConnectionMode) {
        connectionMode = mode
        relayMode = mode.legacyRelayMode
    }

    mutating func updateLegacyRelayMode(_ mode: RelayMode) {
        updateConnectionMode(RelayConnectionMode(legacyRelayMode: mode))
    }

    static func defaultValue(
        buildConfiguration: AppBuildConfiguration = .current(),
        environmentPolicy: AppEnvironmentPolicy = .currentBuild
    ) -> RelayConfiguration {
        RelayConfiguration(
            connectionMode: .selfHostedRelay,
            customRelayBaseURL: environmentPolicy.allowsEnvironmentOverrides ? AppEnvironment.development.baseURLString : "",
            hostedRelayBaseURL: buildConfiguration.hostedRelayBaseURL,
            hostedRelayEnabled: buildConfiguration.hostedRelayEnabled
        )
    }

    static func migratedLegacyValue(
        environment: AppEnvironment,
        buildConfiguration: AppBuildConfiguration = .current(),
        environmentPolicy: AppEnvironmentPolicy = .currentBuild
    ) -> RelayConfiguration {
        if environmentPolicy.allowsEnvironmentOverrides, environment != .production {
            return RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: environment.baseURLString,
                hostedRelayBaseURL: buildConfiguration.hostedRelayBaseURL,
                hostedRelayEnabled: buildConfiguration.hostedRelayEnabled
            )
        }

        return RelayConfiguration.defaultValue(
            buildConfiguration: buildConfiguration,
            environmentPolicy: environmentPolicy
        )
    }

    mutating func applyBuildConfiguration(_ buildConfiguration: AppBuildConfiguration) {
        hostedRelayBaseURL = buildConfiguration.hostedRelayBaseURL
        hostedRelayEnabled = buildConfiguration.hostedRelayEnabled && hostedRelayBaseURL != nil
        customRelayBaseURL = RelayConfiguration.normalizeBaseURL(customRelayBaseURL) ?? customRelayBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        relayMode = connectionMode.legacyRelayMode
    }

    var selectableConnectionModes: [RelayConnectionMode] {
        [.tailscale, .selfHostedRelay]
    }

    var activeBaseURLString: String? {
        RelayConfiguration.normalizeBaseURL(customRelayBaseURL)
    }

    var relayOriginLabel: String {
        guard let baseURLString = activeBaseURLString, let url = URL(string: baseURLString) else {
            return "Not Configured"
        }
        return url.host ?? baseURLString
    }

    var validationMessage: String? {
        let trimmed = customRelayBaseURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "Enter your relay URL." }
        guard RelayConfiguration.normalizeBaseURL(trimmed) != nil else {
            return "Relay URL must be an absolute https:// URL ending with /v1 (plain http:// is only allowed for localhost and LAN addresses)."
        }
        return nil
    }

    static func normalizeBaseURL(_ raw: String?) -> String? {
        guard let raw else { return nil }
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }

        if !trimmed.hasPrefix("http://"), !trimmed.hasPrefix("https://") {
            return nil
        }

        while trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }

        guard var components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased(),
              ["http", "https"].contains(scheme)
        else {
            return nil
        }

        // Plaintext HTTP is only accepted for loopback hosts. Allowing http://
        // over the public internet would expose bearer tokens, pairing codes,
        // and all user chat content to any on-path observer. Tailscale and
        // managed relays are always HTTPS; users wanting to test a local relay
        // can reach it via 127.0.0.1 / localhost.
        if scheme == "http", !RelayConfiguration.isLoopbackHost(components.host) {
            return nil
        }

        let normalizedPath: String
        switch components.path {
        case "", "/":
            normalizedPath = "/v1"
        default:
            normalizedPath = components.path.hasSuffix("/") ? String(components.path.dropLast()) : components.path
        }
        guard normalizedPath.hasSuffix("/v1") else {
            return nil
        }
        components.path = normalizedPath
        return components.string
    }

    private static func isLoopbackHost(_ host: String?) -> Bool {
        guard let host = host?.lowercased() else { return false }
        return host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || isPrivateNetworkHost(host)
    }

    /// RFC1918 private network ranges — allowed over HTTP for LAN relays.
    private static func isPrivateNetworkHost(_ host: String) -> Bool {
        guard let octets = host.split(separator: ".").compactMap({ Int($0) }) as [Int]?,
              octets.count == 4,
              octets.allSatisfy({ (0...255).contains($0) })
        else { return false }
        // 10.0.0.0/8
        if octets[0] == 10 { return true }
        // 172.16.0.0/12
        if octets[0] == 172, (16...31).contains(octets[1]) { return true }
        // 192.168.0.0/16
        if octets[0] == 192, octets[1] == 168 { return true }
        return false
    }
}

struct AppEnvironmentPolicy: Equatable, Sendable {
    let allowsEnvironmentOverrides: Bool

    var availableEnvironments: [AppEnvironment] {
        allowsEnvironmentOverrides ? AppEnvironment.allCases : [.production]
    }

    var defaultEnvironment: AppEnvironment {
        .production
    }

    func sanitize(_ settings: UserSettings) -> UserSettings {
        var sanitized = settings
        if !availableEnvironments.contains(sanitized.environment) {
            sanitized.environment = defaultEnvironment
        }
        return sanitized
    }

    static let currentBuild: AppEnvironmentPolicy = {
        #if DEBUG
        AppEnvironmentPolicy(allowsEnvironmentOverrides: true)
        #else
        AppEnvironmentPolicy(allowsEnvironmentOverrides: false)
        #endif
    }()
}

enum LocationSyncPreference: String, Codable, Hashable, Sendable {
    case foregroundOnly
    case backgroundAllowed

    var displayLabel: String {
        switch self {
        case .foregroundOnly: "Foreground Only"
        case .backgroundAllowed: "Background Allowed"
        }
    }
}

/// The chat background a user has selected.
///
/// Every case is rendered procedurally in SwiftUI rather than shipped as a baked
/// image asset — gradients/textures are `LinearGradient`/`RadialGradient`/`Canvas`
/// drawing, keyed off this enum. See `ChatWallpaperBackground` in
/// `Core/Theme.swift`, which is the single rendering primitive later tasks
/// (chat wallpaper rendering, wallpaper picker) should consume for every case,
/// at any frame size (full-screen background or a small picker thumbnail).
///
/// There is intentionally no `thumbnailName`/asset-name property: nothing here
/// is backed by a named image in the asset catalog. `.custom` carries the raw
/// image bytes the user picked from their photo library.
enum ChatWallpaper: Codable, Equatable, Hashable, Identifiable, Sendable {
    case `default`
    case gradient1, gradient2, gradient3, gradient4
    case texture1, texture2
    case solid
    case custom(Data)

    var id: String {
        switch self {
        case .default: "default"
        case .gradient1: "gradient1"
        case .gradient2: "gradient2"
        case .gradient3: "gradient3"
        case .gradient4: "gradient4"
        case .texture1: "texture1"
        case .texture2: "texture2"
        case .solid: "solid"
        case .custom: "custom"
        }
    }

    var label: String {
        switch self {
        case .default: "Default"
        case .gradient1: "Sunset"
        case .gradient2: "Ocean"
        case .gradient3: "Forest"
        case .gradient4: "Aurora"
        case .texture1: "Paper"
        case .texture2: "Noise"
        case .solid: "Solid"
        case .custom: "Photo"
        }
    }
}

enum ReasoningEffort: String, Codable, CaseIterable, Hashable, Sendable {
    case off
    case low
    case medium
    case high

    var displayLabel: String {
        switch self {
        case .off: "Off"
        case .low: "Low"
        case .medium: "Medium"
        case .high: "High"
        }
    }
}

struct UserSettings: Codable, Hashable, Sendable {
    var userName: String
    var avatarInitials: String
    var notificationsEnabled: Bool
    var hapticFeedbackEnabled: Bool
    var environment: AppEnvironment
    var relayConfiguration: RelayConfiguration
    var autoConnectOnLaunch: Bool
    var locationSyncPreference: LocationSyncPreference
    var themePreset: ThemePreset
    var colorSchemePreference: ColorSchemePreference
    var chatWallpaper: ChatWallpaper
    var showAllDevices: Bool
    var ttsEnabled: Bool
    var ttsVoice: String
    var ttsAutoSpeak: Bool
    var ttsAutoSpeakDuringStreaming: Bool
    var ttsAppleRate: Float
    var ttsAppleVoiceIdentifier: String
    var enterToSend: Bool
    var showReasoning: Bool
    var useStreaming: Bool
    var reasoningEffort: ReasoningEffort
    var dashboardURL: String?
    var dashboardUsername: String?
    var dashboardPassword: String?

    init(
        userName: String = "User",
        avatarInitials: String = "U",
        notificationsEnabled: Bool = true,
        hapticFeedbackEnabled: Bool = true,
        environment: AppEnvironment = AppEnvironmentPolicy.currentBuild.defaultEnvironment,
        relayConfiguration: RelayConfiguration = RelayConfiguration.defaultValue(),
        autoConnectOnLaunch: Bool = true,
        locationSyncPreference: LocationSyncPreference = .foregroundOnly,
        themePreset: ThemePreset = .herald,
        colorSchemePreference: ColorSchemePreference = .system,
        chatWallpaper: ChatWallpaper = .default,
        showAllDevices: Bool = false,
        ttsEnabled: Bool = false,
        ttsVoice: String = "Mia",
        ttsAutoSpeak: Bool = false,
        ttsAutoSpeakDuringStreaming: Bool = true,
        ttsAppleRate: Float = 1.0,
        ttsAppleVoiceIdentifier: String = "com.apple.ttsbundle.siri_female_en-US_compact",
        enterToSend: Bool = false,
        showReasoning: Bool = true,
        useStreaming: Bool = true,
        reasoningEffort: ReasoningEffort = .medium,
        dashboardURL: String? = nil,
        dashboardUsername: String? = nil,
        dashboardPassword: String? = nil
    ) {
        self.userName = userName
        self.avatarInitials = avatarInitials
        self.notificationsEnabled = notificationsEnabled
        self.hapticFeedbackEnabled = hapticFeedbackEnabled
        self.environment = environment
        self.relayConfiguration = relayConfiguration
        self.autoConnectOnLaunch = autoConnectOnLaunch
        self.locationSyncPreference = locationSyncPreference
        self.themePreset = themePreset
        self.colorSchemePreference = colorSchemePreference
        self.chatWallpaper = chatWallpaper
        self.showAllDevices = showAllDevices
        self.ttsEnabled = ttsEnabled
        self.ttsVoice = ttsVoice
        self.ttsAutoSpeak = ttsAutoSpeak
        self.ttsAutoSpeakDuringStreaming = ttsAutoSpeakDuringStreaming
        self.ttsAppleRate = ttsAppleRate
        self.ttsAppleVoiceIdentifier = ttsAppleVoiceIdentifier
        self.enterToSend = enterToSend
        self.showReasoning = showReasoning
        self.useStreaming = useStreaming
        self.reasoningEffort = reasoningEffort
        self.dashboardURL = dashboardURL
        self.dashboardUsername = dashboardUsername
        self.dashboardPassword = dashboardPassword
    }

    private enum CodingKeys: String, CodingKey {
        case userName
        case avatarInitials
        case notificationsEnabled
        case hapticFeedbackEnabled
        case environment
        case relayConfiguration
        case autoConnectOnLaunch
        case locationSyncPreference
        case themePreset
        case colorSchemePreference
        case chatWallpaper
        case showAllDevices
        case ttsEnabled
        case ttsVoice
        case ttsAutoSpeak
        case ttsAutoSpeakDuringStreaming
        case ttsAppleRate
        case ttsAppleVoiceIdentifier
        case enterToSend
        case showReasoning
        case useStreaming
        case reasoningEffort
        case dashboardURL
        case dashboardUsername
        case dashboardPassword
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        userName = try container.decodeIfPresent(String.self, forKey: .userName) ?? "User"
        avatarInitials = try container.decodeIfPresent(String.self, forKey: .avatarInitials) ?? "U"
        notificationsEnabled = try container.decodeIfPresent(Bool.self, forKey: .notificationsEnabled) ?? true
        hapticFeedbackEnabled = try container.decodeIfPresent(Bool.self, forKey: .hapticFeedbackEnabled) ?? true
        environment = try container.decodeIfPresent(AppEnvironment.self, forKey: .environment) ?? AppEnvironmentPolicy.currentBuild.defaultEnvironment
        relayConfiguration = try container.decodeIfPresent(RelayConfiguration.self, forKey: .relayConfiguration)
            ?? RelayConfiguration.migratedLegacyValue(environment: environment)
        autoConnectOnLaunch = try container.decodeIfPresent(Bool.self, forKey: .autoConnectOnLaunch) ?? true
        locationSyncPreference = try container.decodeIfPresent(LocationSyncPreference.self, forKey: .locationSyncPreference) ?? .foregroundOnly
        // Migration: ThemePreset.nous was renamed to .herald in HERALD 1.0.0.
        // Devices that stored "nous" in UserDefaults will decode it as .herald.
        if var storedRawValue = try container.decodeIfPresent(String.self, forKey: .themePreset) {
            if storedRawValue == "nous" { storedRawValue = "herald" }
            themePreset = ThemePreset(rawValue: storedRawValue) ?? .herald
        } else {
            themePreset = .herald
        }
        colorSchemePreference = try container.decodeIfPresent(ColorSchemePreference.self, forKey: .colorSchemePreference) ?? .system
        chatWallpaper = try container.decodeIfPresent(ChatWallpaper.self, forKey: .chatWallpaper) ?? .default
        showAllDevices = try container.decodeIfPresent(Bool.self, forKey: .showAllDevices) ?? false
        ttsEnabled = try container.decodeIfPresent(Bool.self, forKey: .ttsEnabled) ?? false
        ttsVoice = try container.decodeIfPresent(String.self, forKey: .ttsVoice) ?? "Mia"
        ttsAutoSpeak = try container.decodeIfPresent(Bool.self, forKey: .ttsAutoSpeak) ?? false
        ttsAutoSpeakDuringStreaming = try container.decodeIfPresent(Bool.self, forKey: .ttsAutoSpeakDuringStreaming) ?? true
        ttsAppleRate = try container.decodeIfPresent(Float.self, forKey: .ttsAppleRate) ?? 1.0
        ttsAppleVoiceIdentifier = try container.decodeIfPresent(String.self, forKey: .ttsAppleVoiceIdentifier) ?? "com.apple.ttsbundle.siri_female_en-US_compact"
        enterToSend = try container.decodeIfPresent(Bool.self, forKey: .enterToSend) ?? false
        showReasoning = try container.decodeIfPresent(Bool.self, forKey: .showReasoning) ?? true
        useStreaming = try container.decodeIfPresent(Bool.self, forKey: .useStreaming) ?? true
        reasoningEffort = try container.decodeIfPresent(ReasoningEffort.self, forKey: .reasoningEffort) ?? .medium
        dashboardURL = try container.decodeIfPresent(String.self, forKey: .dashboardURL)
        dashboardUsername = try container.decodeIfPresent(String.self, forKey: .dashboardUsername)
        dashboardPassword = try container.decodeIfPresent(String.self, forKey: .dashboardPassword)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(userName, forKey: .userName)
        try container.encode(avatarInitials, forKey: .avatarInitials)
        try container.encode(notificationsEnabled, forKey: .notificationsEnabled)
        try container.encode(hapticFeedbackEnabled, forKey: .hapticFeedbackEnabled)
        try container.encode(environment, forKey: .environment)
        try container.encode(relayConfiguration, forKey: .relayConfiguration)
        try container.encode(autoConnectOnLaunch, forKey: .autoConnectOnLaunch)
        try container.encode(locationSyncPreference, forKey: .locationSyncPreference)
        try container.encode(themePreset, forKey: .themePreset)
        try container.encode(colorSchemePreference, forKey: .colorSchemePreference)
        try container.encode(chatWallpaper, forKey: .chatWallpaper)
        try container.encode(showAllDevices, forKey: .showAllDevices)
        try container.encode(ttsEnabled, forKey: .ttsEnabled)
        try container.encode(ttsVoice, forKey: .ttsVoice)
        try container.encode(ttsAutoSpeak, forKey: .ttsAutoSpeak)
        try container.encode(ttsAutoSpeakDuringStreaming, forKey: .ttsAutoSpeakDuringStreaming)
        try container.encode(ttsAppleRate, forKey: .ttsAppleRate)
        try container.encode(ttsAppleVoiceIdentifier, forKey: .ttsAppleVoiceIdentifier)
        try container.encode(enterToSend, forKey: .enterToSend)
        try container.encode(showReasoning, forKey: .showReasoning)
        try container.encode(useStreaming, forKey: .useStreaming)
        try container.encode(reasoningEffort, forKey: .reasoningEffort)
        try container.encodeIfPresent(dashboardURL, forKey: .dashboardURL)
        try container.encodeIfPresent(dashboardUsername, forKey: .dashboardUsername)
        try container.encodeIfPresent(dashboardPassword, forKey: .dashboardPassword)
    }

    func applyingEnvironmentPolicy(
        _ policy: AppEnvironmentPolicy = .currentBuild,
        buildConfiguration: AppBuildConfiguration = .current()
    ) -> UserSettings {
        var sanitized = policy.sanitize(self)
        sanitized.relayConfiguration.applyBuildConfiguration(buildConfiguration)
        return sanitized
    }
}

enum AppEnvironment: String, Codable, CaseIterable, Hashable, Sendable {
    case production
    case staging
    case development

    var displayLabel: String {
        switch self {
        case .production: "Production"
        case .staging: "Staging"
        case .development: "Development"
        }
    }

    var baseURLString: String {
        switch self {
        case .production: ""  // Use custom relay URL from RelayConfiguration
        case .staging: ""     // Use custom relay URL from RelayConfiguration
        case .development: "http://127.0.0.1:8000/v1"
        }
    }
}
