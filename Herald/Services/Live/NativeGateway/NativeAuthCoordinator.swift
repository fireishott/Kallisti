import Foundation
import CryptoKit
import AuthenticationServices
import SafariServices
import UIKit
import Security
import os

@MainActor
final class NativeAuthCoordinator: NSObject, SFSafariViewControllerDelegate, ASWebAuthenticationPresentationContextProviding {
    private let logger = Logger(subsystem: "net.fihonline.kallisti", category: "NativeAuth")
    /// Resolves the gateway base URL at call time from the app's current relay
    /// configuration. The relay is user-entered during onboarding, so the
    /// coordinator must NOT capture it at construction (a fresh install has no
    /// relay yet and would bake in the localhost fallback forever). Reading it
    /// live means whatever address the user typed is what gets hit.
    private let baseURLProvider: @MainActor () -> String
    private let session: URLSession
    private let secureStore: any SecureStoreProtocol
    private var activeSafari: SFSafariViewController?
    private var activeListener: LoopbackCallbackListener?
    /// Strong reference to the in-flight ASWebAuthenticationSession. The
    /// session is deallocated by the system if the app drops the last strong
    /// reference, which cancels the flow — this property keeps it alive until
    /// the completion handler fires.
    private var activeAuthSession: ASWebAuthenticationSession?
    /// Window used as the presentation anchor for ASWebAuthenticationSession
    /// (required on iPad; harmless on iPhone).
    private weak var presentationAnchorWindow: UIWindow?

    /// Convenience for tests: a fixed host/port.
    convenience init(host: String, port: Int, session: URLSession = .shared, secureStore: any SecureStoreProtocol) {
        self.init(baseURLProvider: {
            port == 443 ? "https://\(host)" : "http://\(host):\(port)"
        }, session: session, secureStore: secureStore)
    }

    /// Primary init: base URL is resolved lazily via `baseURLProvider` (e.g.
    /// from the live relay configuration), so a server entered during
    /// onboarding is honored on the very first login.
    init(baseURLProvider: @escaping @MainActor () -> String, session: URLSession = .shared, secureStore: any SecureStoreProtocol) {
        self.baseURLProvider = baseURLProvider
        self.session = session
        self.secureStore = secureStore
    }

    // MARK: - Public

    /// Resolve the current gateway base URL (live, not cached).
    private var gatewayBaseURL: String { baseURLProvider() }

    func startLogin(presentingFrom viewController: UIViewController) async throws {
        let verifier = Self.randomPKCEVerifier()
        let challenge = Self.s256Challenge(for: verifier)
        let state = Self.randomState()

        let listener = LoopbackCallbackListener()
        activeListener = listener
        try await listener.start()

        var components = URLComponents(string: "\(gatewayBaseURL)/auth/native/authorize")!
        components.queryItems = [
            // Explicit -- the dashboard also has the "basic" password
            // provider registered (dashboard.basic_auth in config.yaml), so
            // the route's auto-select-when-empty logic (routes.py:318-329,
            // only kicks in with exactly one session provider) can't guess;
            // omitting this 404s "Unknown provider: ''" even though nous
            // itself is correctly registered. Confirmed live: provider=nous
            // -> 302, provider omitted -> 404.
            URLQueryItem(name: "provider", value: "nous"),
            URLQueryItem(name: "code_challenge", value: challenge),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "redirect_uri", value: "http://127.0.0.1:\(listener.port)/callback"),
            URLQueryItem(name: "state", value: state),
        ]

        presentationAnchorWindow = viewController.view.window

        // Primary path: ASWebAuthenticationSession (RFC 8252 native-app flow).
        // SFSafariViewController shares Safari's cookie jar, and iOS ITP /
        // iCloud Private Relay partitioning drops the gateway's PKCE cookie
        // across the Nous redirect chain — the server then rejects the
        // callback with "Missing PKCE state cookie". ASWebAuthenticationSession
        // with an EPHEMERAL session keeps a dedicated cookie jar for the whole
        // round trip, immune to that partitioning, so the PKCE cookie set by
        // /auth/native/authorize survives back to /auth/callback.
        do {
            let callbackURL = try await presentAuthSession(url: components.url!)
            guard let callback = Self.parseCallback(from: callbackURL), callback.state == state else {
                throw NativeAuthError.stateMismatch
            }
            activeListener = nil
            logger.info("Callback received via ASWebAuthenticationSession, state matched, exchanging code")
            try await exchangeCode(callback.code, verifier: verifier)
            return
        } catch NativeAuthError.loginCancelled {
            // User explicitly cancelled — do NOT fall back; presenting another
            // browser would be hostile.
            activeListener?.stop()
            activeListener = nil
            throw NativeAuthError.loginCancelled
        } catch {
            // The session failed to start or the loopback callback wasn't
            // captured. Fall back to the proven SFSafariViewController +
            // loopback-listener path rather than failing the login outright.
            logger.info("ASWebAuthenticationSession unavailable (\(error.localizedDescription)); falling back to SFSafariViewController")
            activeAuthSession = nil
            let safari = SFSafariViewController(url: components.url!)
            safari.delegate = self
            activeSafari = safari
            viewController.present(safari, animated: true)
            logger.info("Presented OAuth authorize URL in SFSafariViewController")

            let callback = try await listener.waitForCallback()
            safari.dismiss(animated: true)
            activeSafari = nil
            activeListener = nil

            guard callback.state == state else {
                throw NativeAuthError.stateMismatch
            }
            logger.info("Callback received (SFSVC fallback), state matched, exchanging code for tokens")
            try await exchangeCode(callback.code, verifier: verifier)
        }
    }

    /// Present the authorize URL in ASWebAuthenticationSession and await the
    /// loopback callback URL. The session is ephemeral so the gateway's PKCE
    /// cookie survives the Nous round trip (SFSVC's shared Safari jar drops it
    /// under ITP/Private Relay — the missing_pkce_cookie failures).
    private func presentAuthSession(url: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            let authSession = ASWebAuthenticationSession(
                url: url,
                callbackURLScheme: "http",
                completionHandler: { [weak self] callbackURL, error in
                    Task { @MainActor in
                        self?.activeAuthSession = nil
                        if let error {
                            if let asError = error as? ASWebAuthenticationSessionError,
                               asError.code == .canceledLogin {
                                continuation.resume(throwing: NativeAuthError.loginCancelled)
                            } else {
                                continuation.resume(throwing: error)
                            }
                        } else if let callbackURL {
                            continuation.resume(returning: callbackURL)
                        } else {
                            continuation.resume(throwing: NativeAuthError.callbackParseFailed)
                        }
                    }
                }
            )
            authSession.prefersEphemeralWebBrowserSession = true
            authSession.presentationContextProvider = self
            activeAuthSession = authSession
            let started = authSession.start()
            if !started {
                activeAuthSession = nil
                continuation.resume(throwing: NativeAuthError.authSessionFailed("Couldn't start the sign-in session on this device."))
            }
        }
    }

    nonisolated func safariViewControllerDidFinish(_ controller: SFSafariViewController) {
        Task { @MainActor in
            activeListener?.stop()
            activeListener = nil
        }
    }

    // MARK: - ASWebAuthenticationPresentationContextProviding

    nonisolated func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        // Captured on the main actor at startLogin time; reading a weak ref is
        // safe from any thread.
        let anchor = MainActor.assumeIsolated { self.presentationAnchorWindow }
        return anchor ?? ASPresentationAnchor()
    }

    // MARK: - Token Exchange

    func exchangeCode(_ code: String, verifier: String) async throws {
        var request = URLRequest(url: URL(string: "\(gatewayBaseURL)/auth/native/token")!)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["code": code, "code_verifier": verifier])

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw NativeAuthError.tokenExchangeFailed
        }
        let tokens = try JSONDecoder().decode(NativeTokenResponse.self, from: data)
        await secureStore.store(key: "nativeGatewayAccessToken", value: tokens.accessToken)
        await secureStore.store(key: "nativeGatewayRefreshToken", value: tokens.refreshToken)
        logger.info("Tokens stored in keychain")
    }

    /// The stored gateway access token, for authenticating connector calls
    /// that native-gateway clients make (they have no paired credential).
    func currentAccessToken() async -> String? {
        await secureStore.retrieve(key: "nativeGatewayAccessToken")
    }

    func mintTicket() async throws -> String {
        guard let accessToken = await secureStore.retrieve(key: "nativeGatewayAccessToken") else {
            throw NativeAuthError.notLoggedIn
        }
        var request = URLRequest(url: URL(string: "\(gatewayBaseURL)/api/auth/ws-ticket")!)
        request.httpMethod = "POST"
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw NativeAuthError.ticketMintFailed
        }
        struct TicketResponse: Decodable { let ticket: String }
        let ticket = try JSONDecoder().decode(TicketResponse.self, from: data).ticket
        logger.info("Minted fresh ws-ticket via Bearer auth")
        return ticket
    }

    nonisolated static func s256Challenge(for verifier: String) -> String {
        let digest = SHA256.hash(data: Data(verifier.utf8))
        return Data(digest).base64URLEncodedString()
    }

    // MARK: - Private

    /// Extract ``code`` + ``state`` from the loopback callback URL that
    /// ASWebAuthenticationSession returns (http://127.0.0.1:<port>/callback?code=...&state=...).
    private static func parseCallback(from url: URL) -> (code: String, state: String)? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let code = components.queryItems?.first(where: { $0.name == "code" })?.value,
              let state = components.queryItems?.first(where: { $0.name == "state" })?.value else {
            return nil
        }
        return (code, state)
    }

    private static func randomPKCEVerifier() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return Data(bytes).base64URLEncodedString()
    }

    private static func randomState() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return Data(bytes).base64URLEncodedString()
    }
}

// MARK: - Supporting Types

private struct NativeTokenResponse: Decodable {
    let accessToken: String
    let refreshToken: String
    let expiresAt: Int

    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case expiresAt = "expires_at"
    }
}

enum NativeAuthError: Error {
    case listenerSetupFailed
    case invalidBaseURL(String)
    case loginCancelled
    case callbackParseFailed
    case stateMismatch
    case tokenExchangeFailed
    case notLoggedIn
    case ticketMintFailed
    /// The ASWebAuthenticationSession could not be started on this device.
    case authSessionFailed(String)
    /// Nous sign-in itself succeeded, but the follow-up gateway connection
    /// (ws-ticket + WebSocket verification) didn't. Distinct from
    /// tokenExchangeFailed, which is the OAuth step itself failing.
    case connectFailedAfterLogin
}

extension NativeAuthError: LocalizedError {
    /// Without this, NSError's fallback ("The operation couldn't be
    /// completed. (Kallisti.NativeAuthError error N.)") is what reaches
    /// OnboardingFlowView's "Hermes sign-in failed: ..." message.
    var errorDescription: String? {
        switch self {
        case .listenerSetupFailed:
            return "Couldn't start the sign-in listener on this device."
        case .invalidBaseURL(let value):
            return "Invalid gateway address: \(value)"
        case .loginCancelled:
            return "Sign-in was cancelled."
        case .callbackParseFailed:
            return "Sign-in callback couldn't be read."
        case .stateMismatch:
            return "Sign-in response didn't match this request. Please try again."
        case .tokenExchangeFailed:
            return "Couldn't complete sign-in with the gateway."
        case .notLoggedIn:
            return "Not signed in."
        case .ticketMintFailed:
            return "Couldn't establish a session with the gateway."
        case .authSessionFailed(let reason):
            return reason
        case .connectFailedAfterLogin:
            return "Signed in, but couldn't reach the gateway. Check that your Hermes host is running and reachable, then retry."
        }
    }
}
