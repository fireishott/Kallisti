//
//  SharedRelayConfiguration.swift
//  HeraldSupport
//
//  Reads the relay base URL from the App Group `UserDefaults` so that every
//  Herald binary (main app, widget, control, notification service) sees the
//  same value without launching the host process.  The URL is non-secret by
//  design — only the access token is held in the Keychain.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation
import os

/// Thread-safe accessor for the shared relay base URL.
///
/// `UserDefaults(suiteName:)` is documented as thread-safe; the wrapper
/// is `@unchecked Sendable` for the same reason the legacy
/// `HeraldAppState` used one.
public final class SharedRelayConfiguration: @unchecked Sendable {
    /// Singleton — every consumer process opens the same App Group suite.
    public static let shared = SharedRelayConfiguration()

    private let defaults: UserDefaults?
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "SharedRelayConfiguration")

    public init(suiteName: String = HeraldSupportConfiguration.appGroupIdentifier) {
        self.defaults = UserDefaults(suiteName: suiteName)
        if defaults == nil {
            logger.error("App Group \(suiteName, privacy: .public) unavailable — control surface will report configuration errors")
        }
    }

    /// The relay base URL persisted by the main app after pairing.  Empty
    /// string when the app has never been paired.
    public func relayBaseURL() -> String {
        defaults?.string(forKey: HeraldSupportConfiguration.relayBaseURLDefaultsKey) ?? ""
    }

    /// True when the user has paired the device and the URL is non-empty.
    public var isConfigured: Bool {
        !relayBaseURL().isEmpty
    }

    /// Write the relay base URL.  Called by the main app after pairing;
    /// widget/control binaries only read.
    public func update(relayBaseURL urlString: String) {
        defaults?.set(urlString, forKey: HeraldSupportConfiguration.relayBaseURLDefaultsKey)
    }

    /// Clear the cached URL.  Called on sign-out / disconnect.
    public func clear() {
        defaults?.removeObject(forKey: HeraldSupportConfiguration.relayBaseURLDefaultsKey)
    }
}
