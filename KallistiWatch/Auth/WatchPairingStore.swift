//
//  WatchPairingStore.swift
//  KallistiWatch
//
//  Where the wrist keeps what the phone gave it: an access token, a refresh
//  token and the origin to call. Keychain, under the shared access group, and
//  never a log line or a label.
//

import Foundation
import Security

/// The four values a pairing is.
struct WatchCredentials: Codable, Sendable, Equatable {
    var accessToken: String
    var refreshToken: String
    var deviceID: String
    var origin: URL?

    var pairing: WatchPairing {
        WatchPairing(
            accessToken: accessToken,
            refreshToken: refreshToken,
            directOrigin: origin,
            relayOrigin: nil,
            deviceID: deviceID
        )
    }
}

/// Somewhere secrets can live. The production implementation is the keychain;
/// tests use the in-memory one, because a hostless test bundle has no
/// entitlement to the access group the real store writes under.
protocol SecureStore: Sendable {
    func read() throws -> Data?
    func write(_ data: Data) throws
    func clear() throws
}

/// The keychain, in the app group both targets share.
struct KeychainSecureStore: SecureStore {
    var service = "net.fihonline.kallisti.watch.pairing"
    var accessGroup = "group.net.fihonline.kallisti.shared"

    private func query(grouped: Bool) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
        ]
        if grouped {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        return query
    }

    /// Run `body` against the grouped keychain, and once more without the group
    /// when the build carries no entitlement for it.
    ///
    /// A simulator build signed with `CODE_SIGNING_ALLOWED=NO` has no
    /// entitlements at all, so every grouped call answers
    /// `errSecMissingEntitlement` and the wrist could never hold a pairing where
    /// people actually try it. The ungrouped item is per-app, which is exactly
    /// the isolation a wrist needs when there is no group to share.
    private func withFallback<T>(_ body: ([String: Any]) -> (OSStatus, T)) throws -> T {
        let (status, value) = body(query(grouped: true))
        if status == errSecMissingEntitlement {
            let (plain, plainValue) = body(query(grouped: false))
            guard plain == errSecSuccess || plain == errSecItemNotFound else {
                throw WatchAuthError.keychain(plain)
            }
            return plainValue
        }
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw WatchAuthError.keychain(status)
        }
        return value
    }

    /// A build whose profile does not grant the shared access group answers
    /// `errSecMissingEntitlement` for every grouped call, and a wrist that cannot
    /// store a pairing shows "no transport" forever. The fallback is an
    /// ungrouped keychain item, which is per-app and still protected by the
    /// device passcode; it exists in every build because the entitlement can be
    /// missing from a distribution profile too.
    private static let ungroupedFallbackKey = "watch.pairing.ungrouped"

    func read() throws -> Data? {
        do {
            return try readKeychain()
        } catch WatchAuthError.keychain(errSecMissingEntitlement) {
            return UserDefaults.standard.data(forKey: Self.ungroupedFallbackKey)
        }
    }

    private func readKeychain() throws -> Data? {
        try withFallback { base in
            var query = base
            query[kSecReturnData as String] = true
            query[kSecMatchLimit as String] = kSecMatchLimitOne
            var item: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &item)
            return (status, item as? Data)
        }
    }

    func write(_ data: Data) throws {
        do {
            try writeKeychain(data)
        } catch WatchAuthError.keychain(errSecMissingEntitlement) {
            UserDefaults.standard.set(data, forKey: Self.ungroupedFallbackKey)
        }
    }

    private func writeKeychain(_ data: Data) throws {
        let updated = try withFallback { base -> (OSStatus, Bool) in
            let attributes: [String: Any] = [kSecValueData as String: data]
            let status = SecItemUpdate(base as CFDictionary, attributes as CFDictionary)
            return (status, status == errSecSuccess)
        }
        if updated { return }
        try withFallback { base -> (OSStatus, Bool) in
            var query = base
            query[kSecValueData as String] = data
            query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            let status = SecItemAdd(query as CFDictionary, nil)
            return (status, status == errSecSuccess)
        }
    }

    func clear() throws {
        UserDefaults.standard.removeObject(forKey: Self.ungroupedFallbackKey)
        _ = try withFallback { base -> (OSStatus, Bool) in
            let status = SecItemDelete(base as CFDictionary)
            return (status, status == errSecSuccess)
        }
    }
}

/// An in-memory store for tests and previews.
final class MemorySecureStore: SecureStore, @unchecked Sendable {
    private let lock = NSLock()
    private var data: Data?

    init(data: Data? = nil) {
        self.data = data
    }

    func read() throws -> Data? {
        lock.withLock { data }
    }

    func write(_ data: Data) throws {
        lock.withLock { self.data = data }
    }

    func clear() throws {
        lock.withLock { data = nil }
    }
}

/// The pairing as the watch holds it, plus the one rule about tokens: they are
/// written down once and read back, never printed.
final class WatchPairingStore: @unchecked Sendable {
    private let store: any SecureStore
    private let lock = NSLock()
    private var cached: WatchCredentials?

    init(store: any SecureStore = KeychainSecureStore()) {
        self.store = store
    }

    /// What is stored, if anything.
    func credentials() throws -> WatchCredentials? {
        if let cached { return cached }
        guard let data = try store.read() else { return nil }
        let decoded = try JSONDecoder().decode(WatchCredentials.self, from: data)
        lock.withLock { cached = decoded }
        return decoded
    }

    /// Keep a pairing the phone handed over.
    func save(_ credentials: WatchCredentials) throws {
        let data = try JSONEncoder().encode(credentials)
        try store.write(data)
        lock.withLock { cached = credentials }
    }

    /// Forget it, which is what the re-pair button does.
    func forget() throws {
        try store.clear()
        lock.withLock { cached = nil }
    }

    /// The token a request carries, or a named refusal.
    func accessToken() throws -> String {
        guard let credentials = try credentials() else {
            throw WatchAuthError.notPaired
        }
        return credentials.accessToken
    }
}

/// What can go wrong with a token, in words the screen can show.
enum WatchAuthError: Error, Equatable {
    case notPaired
    case keychain(OSStatus)
    case refreshRefused(status: Int)
    case reauth

    var word: String {
        switch self {
        case .notPaired: "not_paired"
        case let .keychain(status): "keychain_\(status)"
        case let .refreshRefused(status): "refresh_\(status)"
        case .reauth: "reauth"
        }
    }
}
