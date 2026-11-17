import Foundation

// MARK: - Stored tokens

/// What the app keeps between launches: where the daemon is, and the pair it
/// minted for this device.
///
/// The base URL is stored beside the tokens because a paired phone has to reach
/// the same listener after a relaunch without re-reading a QR code. `expiresAt`
/// is persisted as the daemon's own RFC 3339 string so the on-disk blob does not
/// depend on a JSON encoder's date strategy.
struct ErisStoredTokens: Equatable, Sendable {
    /// The device listener this pair belongs to.
    let baseURL: URL
    /// Bearer access token.
    let accessToken: String
    /// Refresh token, when the daemon issued one.
    let refreshToken: String?
    /// Device id the daemon bound this pair to.
    let deviceID: String?
    /// Scopes the pair carries.
    let scopes: [String]
    /// Access token expiry.
    let expiresAt: Date?

    init(baseURL: URL, tokens: ErisTokenPair) {
        self.baseURL = baseURL
        self.accessToken = tokens.accessToken
        self.refreshToken = tokens.refreshToken
        self.deviceID = tokens.deviceID
        self.scopes = tokens.scopes
        self.expiresAt = tokens.expiresAt
    }

    /// The same values as a wire-shaped pair.
    var tokens: ErisTokenPair {
        ErisTokenPair(
            accessToken: accessToken,
            refreshToken: refreshToken,
            deviceID: deviceID,
            scopes: scopes,
            expiresAt: expiresAt
        )
    }
}

extension ErisStoredTokens: Codable {
    private enum CodingKeys: String, CodingKey {
        case baseURL = "base_url"
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case deviceID = "device_id"
        case scopes
        case expiresAt = "expires_at"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let baseURLString = try container.decode(String.self, forKey: .baseURL)
        guard let baseURL = URL(string: baseURLString) else {
            throw ErisError.decoding("stored base URL \(baseURLString) is not a URL")
        }
        self.baseURL = baseURL
        self.accessToken = try container.decode(String.self, forKey: .accessToken)
        self.refreshToken = try container.decodeIfPresent(String.self, forKey: .refreshToken)
        self.deviceID = try container.decodeIfPresent(String.self, forKey: .deviceID)
        self.scopes = try container.decodeIfPresent([String].self, forKey: .scopes) ?? []
        self.expiresAt = ErisTimestamp.date(from: try container.decodeIfPresent(String.self, forKey: .expiresAt))
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(baseURL.absoluteString, forKey: .baseURL)
        try container.encode(accessToken, forKey: .accessToken)
        try container.encodeIfPresent(refreshToken, forKey: .refreshToken)
        try container.encodeIfPresent(deviceID, forKey: .deviceID)
        try container.encode(scopes, forKey: .scopes)
        try container.encodeIfPresent(expiresAt.map(ErisTimestamp.string(from:)), forKey: .expiresAt)
    }
}

// MARK: - Token store

/// Where `ErisClient` gets its bearer and its refresh token.
///
/// `@MainActor` because the existing `KeychainSecureStore` is, and the transport
/// must not need a second keychain wrapper. A test substitutes an in-memory
/// conformer and never touches the keychain or the network.
@MainActor
protocol ErisTokenStoring: Sendable {
    /// The stored pair, or `nil` when this device has never paired.
    func load() async throws -> ErisStoredTokens?
    /// Persist a pair, replacing whatever was stored.
    func save(_ tokens: ErisStoredTokens) async throws
    /// Forget the pair (unpair).
    func clear() async
}

/// Keychain-backed token storage over the app's existing `KeychainSecureStore`.
///
/// Same service name as the rest of the app, one dedicated account key, and the
/// `ThisDeviceOnly` accessibility `KeychainSecureStore` already sets: a bearer
/// that refreshes a session on a daemon must not ride an iCloud backup to
/// another device.
@MainActor
final class KeychainErisTokenStore: ErisTokenStoring {
    /// The service name the app already uses for its session material.
    static let defaultServiceName = "net.fihonline.kallisti.session"
    /// The keychain account this store owns.
    static let defaultKey = "eris.tokens.v1"

    private let secureStore: any SecureStoreProtocol
    private let key: String

    /// Build a store over any `SecureStoreProtocol`.
    init(secureStore: any SecureStoreProtocol, key: String = KeychainErisTokenStore.defaultKey) {
        self.secureStore = secureStore
        self.key = key
    }

    /// Build a store over the app's real keychain.
    convenience init(
        serviceName: String = KeychainErisTokenStore.defaultServiceName,
        key: String = KeychainErisTokenStore.defaultKey
    ) {
        self.init(secureStore: KeychainSecureStore(serviceName: serviceName), key: key)
    }

    func load() async throws -> ErisStoredTokens? {
        guard let text = await secureStore.retrieve(key: key) else { return nil }
        guard let data = text.data(using: .utf8) else {
            throw ErisError.storage("the stored Eris tokens are not UTF-8")
        }
        do {
            return try JSONDecoder().decode(ErisStoredTokens.self, from: data)
        } catch {
            // An unreadable credential is not a credential: drop it so the
            // device can pair again instead of being stuck behind a blob that
            // can never decode.
            await secureStore.delete(key: key)
            return nil
        }
    }

    func save(_ tokens: ErisStoredTokens) async throws {
        let data: Data
        do {
            data = try JSONEncoder().encode(tokens)
        } catch {
            throw ErisError.storage("the Eris tokens did not encode: \(error.localizedDescription)")
        }
        guard let text = String(data: data, encoding: .utf8) else {
            throw ErisError.storage("the encoded Eris tokens are not UTF-8")
        }
        guard await secureStore.store(key: key, value: text) else {
            throw ErisError.storage("the keychain refused the Eris token write")
        }
    }

    func clear() async {
        await secureStore.delete(key: key)
    }
}
