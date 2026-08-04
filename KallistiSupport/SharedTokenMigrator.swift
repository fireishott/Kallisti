//
//  SharedTokenMigrator.swift
//  HeraldSupport
//
//  One-shot helper that copies the relay access token out of the legacy
//  App-Group `UserDefaults` (or the legacy single-app Keychain) and into
//  the shared Keychain access group that every Herald binary reads from.
//  Reads back the new entry before deleting the legacy copy, and never
//  logs the token value.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation
import Security
import os

/// Result of a token migration.  Cases are deliberately narrow so the UI
/// can decide whether to surface "paired" or "needs re-pair".
public enum SharedTokenMigrationResult: Equatable, Sendable {
    /// Token was already in the shared Keychain; nothing to do.
    case alreadyMigrated
    /// Token was copied from the legacy source and verified by read-back.
    case migrated(source: MigrationSource)
    /// No token was present anywhere — caller should treat the device as
    /// unpaired.
    case nothingToMigrate
    /// Migration was attempted but the read-back failed; the legacy entry
    /// was left in place so the user is not silently signed out.
    case failed(reason: String)
}

extension SharedTokenMigrationResult: CustomStringConvertible {
    public var description: String {
        switch self {
        case .alreadyMigrated: return "already-migrated"
        case .migrated(let source): return "migrated(\(source))"
        case .nothingToMigrate: return "nothing-to-migrate"
        case .failed(let reason): return "failed(\(reason))"
        }
    }
}

/// Where the migrated token was found.
public enum MigrationSource: String, Sendable, Equatable {
    /// App-Group `UserDefaults` key `herald.accessToken` — pre-Build-108
    /// storage.  Less secure but what the legacy Controls extension used.
    case appGroupDefaults
    /// Single-app Keychain service `net.fihonline.herald.session` with
    /// no access group.  Pre-Build-108 main-app storage.
    case legacyKeychain
}

/// Migrates the relay access token into the shared Keychain access group.
///
/// Designed to be called once at app launch (or from a Settings → "Move
/// token to secure storage" button).  Idempotent — calling it twice is
/// safe; the second call returns `.alreadyMigrated`.
public struct SharedTokenMigrator {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "SharedTokenMigration")
    private let destination: SharedCredentialProvider
    private let appGroupSuite: String
    private let legacyServiceName: String

    public init(
        destination: SharedCredentialProvider = .shared,
        appGroupSuite: String = KallistiSupportConfiguration.appGroupIdentifier,
        legacyServiceName: String = KallistiSupportConfiguration.keychainServiceName
    ) {
        self.destination = destination
        self.appGroupSuite = appGroupSuite
        self.legacyServiceName = legacyServiceName
    }

    /// Run the migration.  Returns a `SharedTokenMigrationResult` so the
    /// caller can branch on the outcome without logging the token itself.
    public func migrate() -> SharedTokenMigrationResult {
        // Step 1 — confirm the destination already has the token (idempotent).
        if let existing = (try? destination.accessToken()) ?? nil, !existing.isEmpty {
            return .alreadyMigrated
        }

        // Step 2 — look for a legacy copy.
        guard let legacy = readLegacyToken() else {
            return .nothingToMigrate
        }
        let source = legacy.source
        let token = legacy.token

        // Step 3 — write to the shared Keychain access group.
        do {
            try destination.setAccessToken(token)
        } catch {
            logger.error("Migration write failed: \(error.localizedDescription)")
            return .failed(reason: "Could not write the token to the shared Keychain.")
        }

        // Step 4 — verify by reading back.  Never log the value.
        let readBack = (try? destination.accessToken()) ?? nil
        guard let readBack, !readBack.isEmpty, readBack == token else {
            logger.error("Migration read-back mismatch — legacy token left in place")
            return .failed(reason: "Read-back did not match the source token.")
        }

        // Step 5 — only now delete the legacy copy.
        deleteLegacyToken(at: source)
        logger.info("Migration complete; source=\(source.rawValue, privacy: .public)")
        return .migrated(source: source)
    }

    // MARK: - Legacy read

    private func readLegacyToken() -> (source: MigrationSource, token: String)? {
        if let token = readLegacyAppGroupToken(), !token.isEmpty {
            return (.appGroupDefaults, token)
        }
        if let token = readLegacyKeychainToken(), !token.isEmpty {
            return (.legacyKeychain, token)
        }
        return nil
    }

    private func readLegacyAppGroupToken() -> String? {
        let defaults = UserDefaults(suiteName: appGroupSuite)
        return defaults?.string(forKey: "herald.accessToken")
    }

    private func readLegacyKeychainToken() -> String? {
        // The legacy keychain entry uses the same service name but no
        // access group.  Read it as a single-app item.
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: legacyServiceName,
            kSecAttrAccount as String: KallistiSupportConfiguration.keychainAccessTokenAccount,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    // MARK: - Legacy delete

    private func deleteLegacyToken(at source: MigrationSource) {
        switch source {
        case .appGroupDefaults:
            let defaults = UserDefaults(suiteName: appGroupSuite)
            defaults?.removeObject(forKey: "herald.accessToken")
        case .legacyKeychain:
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: legacyServiceName,
                kSecAttrAccount as String: KallistiSupportConfiguration.keychainAccessTokenAccount,
            ]
            SecItemDelete(query as CFDictionary)
        }
    }
}
