import Foundation
import Observation

// MARK: - Daemon store

/// Owns the one Eris daemon this device talks to.
///
/// Two facts survive a relaunch and both live here:
///
///   * the daemon's base URL, stored twice on purpose. The token store keeps the
///     authoritative copy beside the pair it belongs to (so a pair can never be
///     used against a different listener), and `UserDefaults` keeps a
///     non-secret copy so the manual-entry field can be pre-filled even after
///     the tokens are gone.
///   * the last session id a turn landed in, so a relaunch can verify a stored
///     token with `GET /v1/sessions/{id}` instead of queueing a throwaway turn.
///
/// A stored token is verified on launch. The client already refreshes once on a
/// 401; if a 401 still escapes after that, the refresh token is dead too and
/// this store drops the pair and asks for a new code. A transport failure is
/// different: the daemon being unreachable does not invalidate a credential, so
/// the pair is kept and the surface says so.
@MainActor
@Observable
final class ErisDaemonStore {
    /// What the surface should show.
    enum Phase: Equatable {
        /// No daemon paired on this device.
        case unpaired
        /// A pairing request is in flight.
        case connecting
        /// Paired to a listener, ready for chat.
        case paired(URL)
    }

    /// The current state.
    private(set) var phase: Phase = .unpaired
    /// The typed `ErisError` message from the last failed action, if any.
    private(set) var failure: String?
    /// The daemon's stable machine code for that failure, if it sent one.
    private(set) var failureCode: String?
    /// A non-fatal condition: the daemon is unreachable but the pair is intact.
    private(set) var notice: String?
    /// Whether the stored pair is being verified right now.
    private(set) var isVerifying = false
    /// The conversation model for the paired daemon, once there is one.
    private(set) var chat: ErisChatViewModel?
    /// The conversation list model for the paired daemon, once there is one.
    private(set) var conversations: ErisConversationsModel?
    /// The settings model for the paired daemon, once there is one.
    private(set) var settings: ErisSettingsModel?
    private(set) var approvals: ErisApprovalsModel?
    /// The read-only health/location model for the paired daemon.
    private(set) var senses: ErisSensesModel?
    /// The durable sensor upload pipeline for the paired daemon.
    private(set) var sensorUpload: SensorUploadService?
    /// The inbox model for the paired daemon.
    private(set) var inbox: ErisInboxModel?
    /// The notes model for the paired daemon.
    private(set) var notes: ErisNotesModel?
    /// The talk readiness model for the paired daemon.
    private(set) var talk: ErisTalkModel?

    /// Manual-entry daemon address, pre-filled from the last pairing.
    var addressText: String
    /// Manual-entry pairing code.
    var codeText: String
    /// The device name the daemon records for this phone.
    var deviceName: String

    /// Where the bearer and refresh token live.
    private let tokenStore: any ErisTokenStoring
    /// Non-secret memory: the daemon address and the last session id.
    private let defaults: UserDefaults
    /// The session the transport uses. Injected so tests never touch a socket.
    private let session: URLSession

    /// Keys this store owns in `UserDefaults`. Address and session id only:
    /// tokens go to the keychain, never here.
    private enum DefaultsKey {
        static let baseURL = "eris.daemon.baseURL"
        static let lastSession = "eris.daemon.lastSessionID"
    }

    /// Build a store over an injected token store and defaults.
    ///
    /// Every dependency has a production default that is not main-actor
    /// isolated, so a `@State` default value can construct this type.
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

    /// The base URL of the paired daemon, when there is one.
    var pairedBaseURL: URL? {
        if case .paired(let url) = phase { return url }
        return nil
    }

    /// The address to show a user, without scheme when it was typed plainly.
    var pairedDisplayAddress: String? {
        guard let url = pairedBaseURL else { return nil }
        let host = url.host ?? url.absoluteString
        if let port = url.port { return "\(host):\(port)" }
        return host
    }

    /// Whether the pairing form's controls should be disabled.
    var isBusy: Bool {
        phase == .connecting || isVerifying
    }

    /// Whether "Forget this daemon" applies.
    var hasStoredDaemon: Bool {
        pairedBaseURL != nil || !addressText.isEmpty
    }

    // MARK: Launch

    /// Restore a stored pair and verify it, so a relaunch lands in chat.
    ///
    /// Safe to call more than once: a second call while already paired is a
    /// no-op, so a view's `.task` and a foreground return cannot double-pair.
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

    /// Verify a stored pair without queueing a real turn.
    private func verify(stored: ErisStoredTokens) async {
        isVerifying = true
        defer { isVerifying = false }

        let client = makeClient(baseURL: stored.baseURL)
        do {
            if let sessionID = defaults.string(forKey: DefaultsKey.lastSession), !sessionID.isEmpty {
                _ = try await client.session(sessionID: sessionID)
            } else {
                // No session to ask about yet: a refresh is the only authorized
                // call that cannot create work on the daemon.
                _ = try await client.refresh()
            }
            notice = nil
        } catch let error as ErisError {
            if error.isUnauthorized {
                // The refresh path inside the client already ran and still
                // produced a 401, so the refresh token is dead as well.
                await dropStoredPair()
                failure = "the pairing expired: \(error.errorDescription ?? "unauthorized")"
                failureCode = error.code
            } else {
                // Unreachable daemon, undecodable body, missing refresh token:
                // the credential may still be good, so keep it and say why.
                notice = error.errorDescription
            }
        } catch {
            notice = error.localizedDescription
        }
    }

    // MARK: Pairing

    /// Redeem a scanned QR payload.
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

    /// Redeem the manually typed address and code.
    ///
    /// A typed `host:port` is accepted as http, because that is how a phone user
    /// copies an address off the daemon's own screen.
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

    /// Give a scheme-less address the http scheme the device listener serves.
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
            // `ErisClient.pair` stored the pair before returning, so the base URL
            // and the tokens are already consistent on disk.
            remember(address: pairing.baseURL)
            codeText = ""
            attach(pairing.baseURL)
        } catch {
            phase = .unpaired
            chat = nil
            conversations = nil
            settings = nil
        approvals = nil
            approvals = nil
            record(error)
        }
    }

    /// Forget the daemon: drop the tokens, the remembered address and the chat.
    func forget() async {
        await dropStoredPair()
        notice = nil
        chat = nil
        addressText = ""
        codeText = ""
    }

    // MARK: Session memory

    /// Remember the session a turn landed in, so the next launch can verify
    /// against it instead of creating a turn to test the token.
    func noteSession(_ sessionID: String) {
        guard !sessionID.isEmpty else { return }
        defaults.set(sessionID, forKey: DefaultsKey.lastSession)
    }

    // MARK: Wiring

    /// Move to paired and build the models for that daemon.
    ///
    /// One client per daemon here, shared by every model, because the pair and the
    /// refresh are one piece of state: two clients would each hold their own view
    /// of the token and race each other's 401 refresh.
    private func attach(_ baseURL: URL) {
        let client = makeClient(baseURL: baseURL)
        chat = ErisChatViewModel(client: client, onSessionID: { [weak self] sessionID in
            self?.noteSession(sessionID)
        })
        conversations = ErisConversationsModel(client: client)
        settings = ErisSettingsModel(client: client)
        approvals = ErisApprovalsModel(client: client)
        senses = ErisSensesModel(client: client)
        inbox = ErisInboxModel(client: client)
        notes = ErisNotesModel(client: client)
        talk = ErisTalkModel(client: client)

        // The sensor pipeline uploads through the same client (and the same
        // bearer/refresh) as turns, so it follows whatever daemon this device is
        // paired to. The device collectors are permission-gated themselves: no
        // capture happens until the user grants health/motion/location on the
        // Permissions screen.
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

    /// A detail model for one conversation on the paired daemon.
    ///
    /// The surface asks for one when a row is tapped, so a transcript model only
    /// exists while a transcript is open.
    func detailModel(conversationID: String, title: String) -> ErisConversationDetailModel? {
        guard let baseURL = pairedBaseURL else { return nil }
        return ErisConversationDetailModel(
            client: makeClient(baseURL: baseURL),
            conversationID: conversationID,
            title: title,
            onSessionID: { [weak self] sessionID in
                self?.noteSession(sessionID)
            }
        )
    }

    private func dropStoredPair() async {
        await tokenStore.clear()
        defaults.removeObject(forKey: DefaultsKey.lastSession)
        defaults.removeObject(forKey: DefaultsKey.baseURL)
        sensorUpload?.stop()
        sensorUpload = nil
        senses = nil
        inbox = nil
        notes = nil
        talk = nil
        chat = nil
        conversations = nil
        settings = nil
        approvals = nil
        phase = .unpaired
    }

    private func makeClient(baseURL: URL) -> ErisClient {
        ErisClient(baseURL: baseURL, tokenStore: tokenStore, session: session)
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
