import Foundation
import Observation

// MARK: - Daemon store

/// Owns the one Eris daemon this device talks to: pairing, the token/base-URL
/// pair, and the sensor upload pipeline. It does NOT own UI models - the 0.4.0
/// stores reach the transport through `ErisHeraldClient` (via
/// `clientForPairedDaemon()`).
@MainActor
@Observable
final class ErisDaemonStore {
    enum Phase: Equatable {
        case unpaired
        case connecting
        case paired(URL)
    }

    private(set) var phase: Phase = .unpaired
    private(set) var failure: String?
    private(set) var failureCode: String?
    private(set) var notice: String?
    private(set) var isVerifying = false
    /// The durable sensor upload pipeline for the paired daemon.
    private(set) var sensorUpload: SensorUploadService?

    var addressText: String
    var codeText: String
    var deviceName: String

    private let tokenStore: any ErisTokenStoring
    private let defaults: UserDefaults
    private let session: URLSession

    private enum DefaultsKey {
        static let baseURL = "eris.daemon.baseURL"
        static let lastSession = "eris.daemon.lastSessionID"
    }

    init(
        tokenStore: (any ErisTokenStoring)? = nil,
        defaults: UserDefaults = .standard,
        session: URLSession = .shared,
        deviceName: String = "Kallisti"
    ) {
        self.tokenStore = tokenStore ?? KeychainErisTokenStore()
        self.defaults = defaults
        self.session = session
        self.deviceName = deviceName
        self.addressText = defaults.string(forKey: DefaultsKey.baseURL) ?? ""
        self.codeText = ""
    }

    // MARK: Derived state

    var pairedBaseURL: URL? {
        if case .paired(let url) = phase { return url }
        return nil
    }

    var isBusy: Bool {
        phase == .connecting || isVerifying
    }

    var hasStoredDaemon: Bool {
        pairedBaseURL != nil || !addressText.isEmpty
    }

    /// A transport for the paired daemon, or nil when none is paired. Callers
    /// share the store's token store *and* one transport instance, so a refresh
    /// is one rotation for the whole app rather than one per caller.
    func clientForPairedDaemon() -> ErisClient? {
        guard let baseURL = pairedBaseURL else { return nil }
        return makeClient(baseURL: baseURL)
    }

    // MARK: Launch

    func start() async {
        if case .paired = phase { return }
        do {
            guard let stored = try await tokenStore.load() else { return }
            remember(address: stored.baseURL)
            attach(stored.baseURL)
            await verify(stored: stored)
        } catch {
            record(error)
        }
    }

    private func verify(stored: ErisStoredTokens) async {
        isVerifying = true
        defer { isVerifying = false }

        let client = makeClient(baseURL: stored.baseURL)
        do {
            // A cheap authenticated read, not a session snapshot. The stored id is a
            // conversation, and `/v1/sessions/{id}` looks up a job session, so the
            // daemon answered a correct 404 on every launch: the pairing looked
            // degraded when it was fine, and the notice filled the chat header.
            _ = try await client.conversations()
            notice = nil
        } catch let error as ErisError {
            if error.isUnauthorized {
                await dropStoredPair()
                failure = "the pairing expired: \(error.errorDescription ?? "unauthorized")"
                failureCode = error.code
            } else {
                notice = error.errorDescription
            }
        } catch {
            notice = error.localizedDescription
        }
    }

    // MARK: Pairing

    func pair(payload: String) async {
        do {
            let pairing = try ErisPairing(payload: payload)
            await pair(with: pairing)
        } catch let error as ErisPairingError {
            clearFailure()
            failure = error.errorDescription
        } catch {
            clearFailure()
            failure = error.localizedDescription
        }
    }

    func pairFromFields() async {
        do {
            let pairing = try ErisPairing(
                baseURLString: Self.normalizedAddress(addressText),
                code: codeText
            )
            await pair(with: pairing)
        } catch let error as ErisPairingError {
            clearFailure()
            failure = error.errorDescription
        } catch {
            clearFailure()
            failure = error.localizedDescription
        }
    }

    nonisolated static func normalizedAddress(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return trimmed }
        guard !trimmed.contains("://") else { return trimmed }
        return "http://" + trimmed
    }

    private func pair(with pairing: ErisPairing) async {
        clearFailure()
        notice = nil
        phase = .connecting
        let client = makeClient(baseURL: pairing.baseURL)
        do {
            _ = try await client.pair(code: pairing.code, deviceName: deviceName)
            remember(address: pairing.baseURL)
            codeText = ""
            attach(pairing.baseURL)
        } catch {
            phase = .unpaired
            record(error)
        }
    }

    func forget() async {
        await dropStoredPair()
        notice = nil
        addressText = ""
        codeText = ""
    }

    // MARK: Session memory

    func noteSession(_ sessionID: String) {
        guard !sessionID.isEmpty else { return }
        defaults.set(sessionID, forKey: DefaultsKey.lastSession)
    }

    // MARK: Wiring

    /// Move to paired and start the sensor pipeline for that daemon.
    private func attach(_ baseURL: URL) {
        let client = makeClient(baseURL: baseURL)

        let locationService = LiveLocationService()
        let healthService = LiveHealthService()
        let motionService = LiveMotionService()
        let upload = SensorUploadService(
            erisClient: client,
            persistence: UserDefaultsAppPersistenceStore(),
            isPairedProvider: { [weak self] in self?.pairedBaseURL != nil },
            locationService: locationService,
            healthService: healthService,
            motionService: motionService
        )
        upload.start()
        sensorUpload = upload

        phase = .paired(baseURL)
    }

    private func dropStoredPair() async {
        await tokenStore.clear()
        transportCache.removeAll()
        defaults.removeObject(forKey: DefaultsKey.lastSession)
        defaults.removeObject(forKey: DefaultsKey.baseURL)
        sensorUpload?.stop()
        sensorUpload = nil
        phase = .unpaired
    }

    /// Forget the remembered daemon address, without touching credentials.
    ///
    /// The unpair path deletes the Keychain pair through `KeychainErisTokenStore`,
    /// but this store also remembers the address in UserDefaults so a relaunch can
    /// reach the listener without re-reading a QR code. Left behind, that address
    /// is half a pairing: an app that knows where a daemon is and cannot sign in
    /// to it. There is no instance to ask here, because the caller may not hold
    /// one - the address is the only thing it left.
    static func clearRememberedAddress(_ defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: DefaultsKey.baseURL)
        defaults.removeObject(forKey: DefaultsKey.lastSession)
    }

    /// One transport per daemon address, kept for the life of the store.
    ///
    /// `ErisClient` is an actor whose refresh is single-flight *within one
    /// instance*. Minting a client per call therefore minted a private
    /// single-flight guard per call: every concurrent 401 refreshed on its own,
    /// the daemon retired each refresh token the moment it rotated it, and every
    /// lane but the first was left presenting a retired token. That is the
    /// 401 storm that ended on "Authentication Failed" and needed a new pairing
    /// code. Cache the instance so "single-flight" is a property of the app and
    /// not of a temporary object.
    private var transportCache: [String: ErisClient] = [:]

    private func makeClient(baseURL: URL) -> ErisClient {
        let key = baseURL.absoluteString
        if let cached = transportCache[key] {
            return cached
        }
        let client = ErisClient(baseURL: baseURL, tokenStore: tokenStore, session: session)
        transportCache[key] = client
        return client
    }

    private func remember(address url: URL) {
        let text = url.absoluteString
        defaults.set(text, forKey: DefaultsKey.baseURL)
        addressText = text
    }

    // MARK: Failures

    private func record(_ error: any Error) {
        guard let erisError = error as? ErisError else {
            failure = error.localizedDescription
            failureCode = nil
            return
        }
        failure = erisError.errorDescription
        failureCode = erisError.code
    }

    private func clearFailure() {
        failure = nil
        failureCode = nil
    }
}
