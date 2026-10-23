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

    init(
        baseURL: URL,
        accessToken: String,
        refreshToken: String?,
        deviceID: String?,
        scopes: [String],
        expiresAt: Date?
    ) {
        self.baseURL = baseURL
        self.accessToken = accessToken
        self.refreshToken = refreshToken
        self.deviceID = deviceID
        self.scopes = scopes
        self.expiresAt = expiresAt
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

/// The non-credential half of a stored pair: everything `ErisStoredTokens`
/// carries that is not a secret. Kept in its own keychain entry so the two
/// credential halves can live in the accounts `AppSessionStore` owns.
private struct ErisPairMetadata: Codable, Equatable {
    let baseURLString: String
    let deviceID: String?
    let scopes: [String]
    let expiresAt: String?

    private enum CodingKeys: String, CodingKey {
        case baseURLString = "base_url"
        case deviceID = "device_id"
        case scopes
        case expiresAt = "expires_at"
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
/// ## One authority for the credentials
///
/// This store used to own a private keychain account (`eris.tokens.v1`) holding
/// a second copy of the bearer and refresh token, while `AppSessionStore` held
/// its own copy in `session.accessToken` / `session.refreshToken`. Both copies
/// refreshed, on **different routes with different semantics**:
///
/// - `AppSessionStore` refreshes through the legacy `/v1/auth/refresh`, which
///   **rotates**: the daemon retires the presented refresh row and mints a new
///   pair.
/// - `ErisClient` refreshes through `/v1/token/refresh`, which deliberately does
///   **not** rotate.
///
/// Whichever refreshed first minted a new row and left the other copy pointing
/// at a retired one, which can never refresh again. The visible symptom was a
/// paired phone that worked until the first refresh and then failed every call
/// with a 401, while the keychain still held a token and looked healthy.
///
/// The fix is to stop having two copies. This store now reads and writes the
/// **same two keychain accounts `AppSessionStore` owns**, on the same service
/// name, so there is exactly one bearer and one refresh token on the device.
/// Rotation cannot desynchronise a copy that does not exist. The non-secret
/// half of the pair (base URL, device id, scopes, expiry) lives in its own
/// metadata entry, because `AppSessionStore` has no field for it.
///
/// The old `eris.tokens.v1` blob is adopted **once**, only when no session
/// access token is present, and then deleted. Adopting it unconditionally would
/// overwrite a fresher token that the session store had already rotated.
@MainActor
final class KeychainErisTokenStore: ErisTokenStoring {
    /// The service name the app already uses for its session material.
    static let defaultServiceName = "net.fihonline.kallisti.session"

    /// The two credential accounts `AppSessionStore` owns. These strings must
    /// stay byte-identical to its private `SecureKeys` values; that identity is
    /// the entire mechanism of this fix.
    static let accessTokenKey = "session.accessToken"
    static let refreshTokenKey = "session.refreshToken"

    /// The non-secret half of the pair.
    static let metadataKey = "eris.tokens.meta.v1"

    /// The single-account blob this store used before the two were unified.
    /// Read once for migration, then deleted. Never written again.
    static let legacyKey = "eris.tokens.v1"

    private let secureStore: any SecureStoreProtocol

    /// Build a store over any `SecureStoreProtocol`.
    init(secureStore: any SecureStoreProtocol) {
        self.secureStore = secureStore
    }

    /// Build a store over the app's real keychain.
    convenience init(serviceName: String = KeychainErisTokenStore.defaultServiceName) {
        self.init(secureStore: KeychainSecureStore(serviceName: serviceName))
    }

    func load() async throws -> ErisStoredTokens? {
        await adoptLegacyPairIfNeeded()

        guard
            let accessToken = await secureStore.retrieve(key: Self.accessTokenKey),
            !accessToken.isEmpty
        else {
            return nil
        }

        let refreshToken = await secureStore.retrieve(key: Self.refreshTokenKey)
        let metadata = await loadMetadata()

        // Without metadata there is no base URL, so the pair cannot build. The
        // credentials are still the session store's, so leaving them in place is
        // correct: only the daemon address is missing, and re-pairing rewrites
        // all three entries.
        guard let metadata, let baseURL = URL(string: metadata.baseURLString) else {
            return nil
        }

        return ErisStoredTokens(
            baseURL: baseURL,
            accessToken: accessToken,
            refreshToken: (refreshToken?.isEmpty == false) ? refreshToken : nil,
            deviceID: metadata.deviceID,
            scopes: metadata.scopes,
            expiresAt: ErisTimestamp.date(from: metadata.expiresAt)
        )
    }

    func save(_ tokens: ErisStoredTokens) async throws {
        let accessOK = await secureStore.store(key: Self.accessTokenKey, value: tokens.accessToken)

        // A nil or empty refresh token must not blank the shared account: the
        // daemon omits `refresh_token` on `/v1/token/refresh`, and the caller
        // has already carried the old one forward. Writing "" here would destroy
        // the only copy on the device.
        var refreshOK = true
        if let refreshToken = tokens.refreshToken, !refreshToken.isEmpty {
            refreshOK = await secureStore.store(key: Self.refreshTokenKey, value: refreshToken)
        }

        guard accessOK, refreshOK else {
            throw ErisError.storage("the keychain refused the Eris token write")
        }

        let metadata = ErisPairMetadata(
            baseURLString: tokens.baseURL.absoluteString,
            deviceID: tokens.deviceID,
            scopes: tokens.scopes,
            expiresAt: tokens.expiresAt.map(ErisTimestamp.string(from:))
        )
        do {
            let data = try JSONEncoder().encode(metadata)
            guard let text = String(data: data, encoding: .utf8) else {
                throw ErisError.storage("the Eris pair metadata is not UTF-8")
            }
            let metadataOK = await secureStore.store(key: Self.metadataKey, value: text)
            guard metadataOK else {
                throw ErisError.storage("the keychain refused the Eris pair metadata write")
            }
        } catch let error as ErisError {
            throw error
        } catch {
            throw ErisError.storage("the Eris pair metadata did not encode: \(error.localizedDescription)")
        }
    }

    func clear() async {
        await secureStore.delete(key: Self.accessTokenKey)
        await secureStore.delete(key: Self.refreshTokenKey)
        await secureStore.delete(key: Self.metadataKey)
        await secureStore.delete(key: Self.legacyKey)
    }

    // MARK: Migration

    /// Adopt the pre-unification `eris.tokens.v1` blob into the session accounts
    /// exactly once, and only when the session store has nothing of its own.
    private func adoptLegacyPairIfNeeded() async {
        guard let text = await secureStore.retrieve(key: Self.legacyKey) else { return }
        defer { Task { await secureStore.delete(key: Self.legacyKey) } }

        // A live session access token is newer than any blob this store wrote,
        // because it has been through a rotation. Leave it alone.
        if let current = await secureStore.retrieve(key: Self.accessTokenKey), !current.isEmpty {
            return
        }

        guard
            let data = text.data(using: .utf8),
            let legacy = try? JSONDecoder().decode(ErisStoredTokens.self, from: data)
        else {
            // An unreadable credential is not a credential. It is already being
            // deleted by the `defer` above; the device can pair again.
            return
        }

        let accessOK = await secureStore.store(key: Self.accessTokenKey, value: legacy.accessToken)
        var refreshOK = true
        if let refreshToken = legacy.refreshToken, !refreshToken.isEmpty {
            refreshOK = await secureStore.store(key: Self.refreshTokenKey, value: refreshToken)
        }
        guard accessOK, refreshOK else { return }

        let metadata = ErisPairMetadata(
            baseURLString: legacy.baseURL.absoluteString,
            deviceID: legacy.deviceID,
            scopes: legacy.scopes,
            expiresAt: legacy.expiresAt.map(ErisTimestamp.string(from:))
        )
        if let encoded = try? JSONEncoder().encode(metadata),
           let metadataText = String(data: encoded, encoding: .utf8) {
            _ = await secureStore.store(key: Self.metadataKey, value: metadataText)
        }
    }

    private func loadMetadata() async -> ErisPairMetadata? {
        guard
            let text = await secureStore.retrieve(key: Self.metadataKey),
            let data = text.data(using: .utf8)
        else {
            return nil
        }
        return try? JSONDecoder().decode(ErisPairMetadata.self, from: data)
    }
}
