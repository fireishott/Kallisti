//
//  SharedTokenBridge.swift
//  Herald
//
//  Bridges the main app's session state to the cross-process support
//  module.  Two responsibilities:
//
//    1. Persist the relay URL + access token to the shared Keychain entry
//       (`SharedCredentialProvider.shared`) so the HeraldControls
//       extension can make authenticated gateway calls without launching
//       the host app.
//
//    2. Migrate any legacy copy of the token (App-Group `UserDefaults`
//       or single-app Keychain) into the shared Keychain on first
//       launch after upgrade.  See Build 108 Phase 3 §15A.4.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation
import HeraldSupport
import os

/// One-stop helper the main app calls after authentication and on
/// sign-out.  Encapsulates the migration + persistence rules so the
/// call sites in `AppContainer` and `AppSessionStore` stay trivial.
enum SharedTokenBridge {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "SharedTokenBridge")
    private static let legacyAppGroupTokenDefaultsKey = "herald.accessToken"

    /// Run the migration once per launch.  Idempotent; safe to call on
    /// every `AppContainer` initialization.
    static func migrateOnce() {
        let migrator = SharedTokenMigrator()
        let result = migrator.migrate()
        logger.info("Shared token migration outcome: \(String(describing: result), privacy: .public)")
    }

    /// Persist the current session's URL + token to the cross-process
    /// surface.  Pass `token: nil` to clear.
    static func publish(relayBaseURL: String?, accessToken: String?) {
        if let url = relayBaseURL, !url.isEmpty {
            SharedRelayConfiguration.shared.update(relayBaseURL: url)
        }
        do {
            try SharedCredentialProvider.shared.setAccessToken(accessToken)
        } catch {
            logger.error("Shared credential publish failed: \(error.localizedDescription)")
        }
        if accessToken == nil {
            // Clear the legacy App-Group plaintext entry if it still exists;
            // the migrator already handles the Keychain side.
            let defaults = UserDefaults(suiteName: HeraldSupportConfiguration.appGroupIdentifier)
            defaults?.removeObject(forKey: legacyAppGroupTokenDefaultsKey)
        }
    }

    /// Clear all shared state.  Called on sign-out / disconnect.
    static func clear() {
        SharedRelayConfiguration.shared.clear()
        do {
            try SharedCredentialProvider.shared.setAccessToken(nil)
        } catch {
            logger.error("Shared credential clear failed: \(error.localizedDescription)")
        }
        SharedGatewayStatus.shared.clear()
    }
}
