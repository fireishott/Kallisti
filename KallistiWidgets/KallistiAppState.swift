import Foundation
import os

/// Shared app state accessible from both the main app and Control Center extension.
/// Uses App Group UserDefaults for cross-process communication so Control Widgets
/// can read the relay URL and access token without launching the main app.
///
/// UserDefaults is documented as thread-safe by Apple, so the shared singleton is
/// safe despite Swift 6 not having a Sendable conformance for it.
final class KallistiAppState: @unchecked Sendable {
    static let shared = KallistiAppState()

    private let defaults: UserDefaults = {
        guard let suite = UserDefaults(suiteName: "group.net.fihonline.kallisti") else {
            fatalError("App Group 'group.net.fihonline.kallisti' not available — check provisioning profile.")
        }
        return suite
    }()
    private let logger = Logger(subsystem: "net.fihonline.kallisti", category: "AppState")

    /// The relay base URL. Set by the main app after pairing.
    /// Widgets read this from shared App Group UserDefaults.
    var relayBaseURL: String {
        defaults.string(forKey: "kallisti.relayBaseURL") ?? ""
    }

    /// The current relay access token, if authenticated.
    var accessToken: String? {
        defaults.string(forKey: "kallisti.accessToken")
    }

    /// True when the main app has an active relay session.
    var isAuthenticated: Bool {
        accessToken != nil
    }

    /// Called by the main app to update shared state after authentication or
    /// relay configuration changes.
    func update(relayBaseURL: String, accessToken: String?) {
        defaults.set(relayBaseURL, forKey: "kallisti.relayBaseURL")
        if let token = accessToken {
            defaults.set(token, forKey: "kallisti.accessToken")
        } else {
            defaults.removeObject(forKey: "kallisti.accessToken")
        }
        logger.debug("App state updated: url=\(relayBaseURL) auth=\(accessToken != nil)")
    }

    /// Clear all shared state (called on sign-out).
    func clear() {
        defaults.removeObject(forKey: "kallisti.relayBaseURL")
        defaults.removeObject(forKey: "kallisti.accessToken")
    }
}
