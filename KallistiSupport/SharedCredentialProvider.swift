//
//  SharedCredentialProvider.swift
//  HeraldSupport
//
//  Reads and writes the relay access token in the shared Keychain access
//  group.  The token is a credential and MUST NOT be persisted in shared
//  plaintext defaults — per Build 108 Phase 3 §15A.4 the legacy
//  App-Group `UserDefaults` storage is migrated to the Keychain on next
//  app launch.
//
//  Build 108 — Phase 3 §15A.
//

import Foundation
import Security
import os

/// Errors thrown by the shared credential provider.  Typed so callers
/// (widget timeline providers, control intents, the migration routine)
/// can distinguish configuration faults from a real missing token.
public enum SharedCredentialError: LocalizedError, Equatable, Sendable {
    /// The shared Keychain access group is not reachable from this process.
    /// Most often caused by a missing entitlement or a Keychain Sharing
    /// capability that was not provisioned in the App ID.
    case accessGroupUnavailable(group: String)
    /// The Keychain rejected the read or write.  Carries the OSStatus so
    /// tests and the migration routine can react to `errSecMissingEntitlement`
    /// vs `errSecItemNotFound` etc.
    case keychainFailure(status: OSStatus, operation: String)
    /// The provider was not configured with an access group before being
    /// asked to read/write.
    case notConfigured

    public var errorDescription: String? {
        switch self {
        case .accessGroupUnavailable(let group):
            return "The shared Keychain access group '\(group)' is not reachable. Check the App ID capabilities."
        case .keychainFailure(let status, let operation):
            return "Keychain \(operation) failed with status \(status)."
        case .notConfigured:
            return "Shared credential provider was not configured before use."
        }
    }
}

/// Read/write the relay access token in the shared Keychain access group.
///
/// All consumers (main app, widget, control, intents, notification service)
/// call this same provider.  Reads return `nil` when no token is stored;
/// writes return the underlying `OSStatus` so the migration routine can
/// decide whether to fall back to the legacy App Group storage.
public final class SharedCredentialProvider: @unchecked Sendable {
    /// Default singleton.  Tests instantiate their own provider with a
    /// mocked service name + access group.
    public static let shared = SharedCredentialProvider()

    private let logger = Logger(subsystem: "net.fihonline.herald", category: "SharedCredential")

    /// The Keychain service name.  Overrideable so legacy stores
    /// (no access group) can still be read by the migration routine.
    public let serviceName: String

    /// The Keychain access group.  When `nil`, the provider reads/writes
    /// items that are private to the current app/extension.  This is what
    /// legacy stores used; migration uses it once to copy the token, then
    /// the shared provider is re-instantiated with the real access group.
    public let accessGroup: String?

    public init(
        serviceName: String = HeraldSupportConfiguration.keychainServiceName,
        accessGroup: String? = HeraldSupportConfiguration.keychainAccessGroup
    ) {
        self.serviceName = serviceName
        self.accessGroup = accessGroup
    }

    // MARK: - Public API

    /// Returns the relay access token, or `nil` when the user has never
    /// paired the device.
    public func accessToken() throws -> String? {
        try readToken()
    }

    /// Persist or remove the relay access token.  Pass `nil` to remove.
    /// Throws on Keychain failure; the token value itself is never logged.
    public func setAccessToken(_ token: String?) throws {
        guard let token, !token.isEmpty else {
            _ = try deleteToken()
            return
        }
        try writeToken(token)
    }

    // MARK: - Internal

    private func readToken() throws -> String? {
        var query = baseQuery(for: HeraldSupportConfiguration.keychainAccessTokenAccount)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)

        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let token = String(data: data, encoding: .utf8) else {
                return nil
            }
            return token
        case errSecItemNotFound:
            return nil
        default:
            throw SharedCredentialError.keychainFailure(status: status, operation: "read")
        }
    }

    private func writeToken(_ token: String) throws {
        let data = Data(token.utf8)
        var query = baseQuery(for: HeraldSupportConfiguration.keychainAccessTokenAccount)
        let updateAttrs: [String: Any] = [kSecValueData as String: data]

        let updateStatus = SecItemUpdate(query as CFDictionary, updateAttrs as CFDictionary)
        if updateStatus == errSecSuccess {
            return
        }
        if updateStatus != errSecItemNotFound {
            throw SharedCredentialError.keychainFailure(status: updateStatus, operation: "update")
        }

        var insertQuery = query
        insertQuery[kSecValueData as String] = data
        // `AfterFirstUnlockThisDeviceOnly` lets BG extensions (notification
        // service, BG refresh) read the token once the device has been
        // unlocked, but never lets iCloud Keychain carry the item to a
        // different device via restore.
        insertQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let addStatus = SecItemAdd(insertQuery as CFDictionary, nil)
        if addStatus != errSecSuccess {
            throw SharedCredentialError.keychainFailure(status: addStatus, operation: "add")
        }
    }

    private func deleteToken() throws {
        let status = SecItemDelete(baseQuery(for: HeraldSupportConfiguration.keychainAccessTokenAccount) as CFDictionary)
        // `errSecItemNotFound` is not an error — we wanted it gone anyway.
        if status != errSecSuccess && status != errSecItemNotFound {
            throw SharedCredentialError.keychainFailure(status: status, operation: "delete")
        }
    }

    private func baseQuery(for account: String) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: account,
        ]
        // The Keychain access group is only honored on device — simulator
        // builds without a Team ID reject `kSecAttrAccessGroup`.  Injecting
        // an empty access group also returns errSecMissingEntitlement, so
        // we only set the attribute when a non-empty group is configured.
        if let accessGroup, !accessGroup.isEmpty {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        return query
    }
}
