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

    /// Signs in using the gateway's local basic provider. The password is sent
    /// only to the HTTPS endpoint and is never persisted on the device.
    /// Redeems a one-time connector pairing code into a dashboard cookie session.
    /// The code is never persisted. `installationID` binds redemption to this iOS install.
    func loginWithPairingCode(_ code: String, installationID: UUID) async throws {
        let normalized = code.uppercased().replacingOccurrences(of: "-", with: "").replacingOccurrences(of: " ", with: "")
        guard PhonePairingCode.isComplete(normalized) else { throw NativeAuthError.invalidCredentials }
        var request = URLRequest(url: URL(string: "\(gatewayBaseURL)/auth/password-login")!)
        request.httpMethod = "POST"
        request.timeoutInterval = 8
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode([
            "provider": "kallisti-pairing",
            "username": installationID.uuidString.lowercased(),
            "password": normalized,
        ])
        let (_, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw NativeAuthError.invalidCredentials
        }
        await secureStore.store(key: "nativeGatewayAuthMode", value: "kallisti-pairing")
        await secureStore.delete(key: "nativeGatewayAccessToken")
        await secureStore.delete(key: "nativeGatewayRefreshToken")
        await secureStore.delete(key: "nativeGatewayAccessTokenExpiresAt")
        logger.info("Signed in with one-time Kallisti pairing code")
    }

    func loginWithBasic(username: String, password: String) async throws {
        guard !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !password.isEmpty else {
            throw NativeAuthError.invalidCredentials
        }
        var request = URLRequest(url: URL(string: "\(gatewayBaseURL)/auth/password-login")!)
        request.httpMethod = "POST"
        request.timeoutInterval = 8
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["provider": "basic", "username": username, "password": password])
        let (_, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw NativeAuthError.invalidCredentials
        }
        await secureStore.store(key: "nativeGatewayAuthMode", value: "basic")
        await secureStore.delete(key: "nativeGatewayAccessToken")
        await secureStore.delete(key: "nativeGatewayRefreshToken")
        await secureStore.delete(key: "nativeGatewayAccessTokenExpiresAt")
        logger.info("Signed in with local basic auth; password was not persisted")
    }

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
            let callback = try await presentAuthSessionAndCaptureLoopback(url: components.url!, listener: listener)
            guard callback.state == state else {
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

    /// Present the authorize URL in ASWebAuthenticationSession and capture the
    /// loopback callback via the LoopbackCallbackListener.
    ///
    /// ASWebAuthenticationSession is used ONLY as the browser surface: an
    /// ephemeral session keeps the gateway's PKCE cookie alive across the Nous
    /// redirect chain (SFSVC's shared Safari jar drops it under ITP/Private
    /// Relay — the missing_pkce_cookie failures). But ASWAS cannot deliver the
    /// callback itself: its Callback API only supports custom schemes and
    /// https(host:path:) universal links — there is NO loopback
    /// http://127.0.0.1:<port> support (ASWebAuthenticationSessionCallback.h).
    /// So the actual gateway code is captured by the loopback listener, which
    /// binds 127.0.0.1 and catches the browser's redirect the same way the
    /// SFSVC path did. Whichever fires first wins: listener success, or
    /// ASWAS cancel/error.
    private func presentAuthSessionAndCaptureLoopback(
        url: URL,
        listener: LoopbackCallbackListener
    ) async throws -> LoopbackCallbackListener.Callback {
        try await withCheckedThrowingContinuation { continuation in
            var didResume = false
            let resumeOnce: (Result<LoopbackCallbackListener.Callback, Error>) -> Void = { result in
                guard !didResume else { return }
                didResume = true
                continuation.resume(with: result)
            }

            // 1) Watch the loopback listener FIRST so a fast gateway redirect
            //    (sub-second after Nous approves) is never missed.
            let listenerTask = Task {
                do {
                    let callback = try await listener.waitForCallback()
                    // Dismiss the auth sheet once the code is in hand.
                    Task { @MainActor in
                        self.activeAuthSession?.cancel()
                        self.activeAuthSession = nil
                    }
                    resumeOnce(.success(callback))
                } catch {
                    resumeOnce(.failure(error))
                }
            }

            // 2) Present ASWAS with the SHARED browser session (ephemeral=false,
            //    the default). The shared jar is what makes Nous's flaky
            //    "login failure" page survivable: a retry rides on the cached
            //    Google/Nous session cookies instead of starting cold.
            //    Ephemeral=true (build 15/16) isolated the jar and turned every
            //    retry into a cold start - and still dropped the gateway PKCE
            //    cookie at least once (18:00:27 missing_pkce_cookie). The
            //    completion handler only fires for errors/cancel - for loopback
            //    redirects there is no callback URL to deliver, so a
            //    "successful" ASWAS completion with no URL is a parse failure.
            let authSession = ASWebAuthenticationSession(
                url: url,
                callbackURLScheme: "http",
                completionHandler: { callbackURL, error in
                    listenerTask.cancel()
                    if let error {
                        if let asError = error as? ASWebAuthenticationSessionError,
                           asError.code == .canceledLogin {
                            resumeOnce(.failure(NativeAuthError.loginCancelled))
                        } else {
                            resumeOnce(.failure(error))
                        }
                    } else if let callbackURL,
                              let parsed = Self.parseCallback(from: callbackURL) {
                        resumeOnce(.success(.init(code: parsed.code, state: parsed.state)))
                    } else {
                        resumeOnce(.failure(NativeAuthError.callbackParseFailed))
                    }
                }
            )
            // Shared session (ephemeral=false): warm cookies across retries.
            authSession.presentationContextProvider = self
            activeAuthSession = authSession
            let started = authSession.start()
            if !started {
                activeAuthSession = nil
                listenerTask.cancel()
                resumeOnce(.failure(NativeAuthError.authSessionFailed("Couldn't start the sign-in session on this device.")))
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
        await secureStore.store(key: "nativeGatewayAccessTokenExpiresAt", value: String(tokens.expiresAt))
        logger.info("Tokens stored in keychain")
    }

    /// The stored gateway access token, for authenticating connector calls
    /// that native-gateway clients make (they have no paired credential).
    func currentAccessToken() async -> String? {
        await secureStore.retrieve(key: "nativeGatewayAccessToken")
    }

    /// True when this session authenticates with the gateway session cookie
    /// (basic / kallisti-pairing password login) rather than a stored native
    /// bearer token. Facade HTTP calls (push registration, native watch) must
    /// mirror mintTicket() and ride the cookie in this mode - login deletes
    /// the keychain access token, so forcing a Bearer either fails outright
    /// or presents a stale token the connector rejects with 401.
    func usesCookieAuth() async -> Bool {
        let authMode = await secureStore.retrieve(key: "nativeGatewayAuthMode")
        return authMode == "basic" || authMode == "kallisti-pairing"
    }

    func mintTicket() async throws -> String {
        let authMode = await secureStore.retrieve(key: "nativeGatewayAuthMode")
        let cookieAuth = authMode == "basic" || authMode == "kallisti-pairing"
        var request = URLRequest(url: URL(string: "\(gatewayBaseURL)/api/auth/ws-ticket")!)
        request.httpMethod = "POST"
        if !cookieAuth {
            let accessToken = try await refreshAccessTokenIfNeeded()
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        }
        // LATENCY (build 34): this HTTP POST used the shared session's
        // default 60s request timeout. On the reconnect path -- triggered by
        // reconnectIfNeeded()'s 5s phantom-socket probe -- a dead/half-open
        // network path (backgrounded app, cell handoff, VPN flap) let this
        // call hang up to 60s BEFORE the WS connect + verify round-trip even
        // started, stacking on top of the (also unbounded) verify below for
        // a combined ~120s dead window with zero UI feedback. Bound it to
        // 8s: generous for a same-network HTTP POST, short enough that a
        // dead path fails fast and scheduleReconnect()'s backoff takes over
        // instead of the user watching a frozen thinking bubble.
        request.timeoutInterval = 8

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw NativeAuthError.ticketMintFailed
        }
        struct TicketResponse: Decodable { let ticket: String }
        let ticket = try JSONDecoder().decode(TicketResponse.self, from: data).ticket
        logger.info("\(cookieAuth ? "Minted fresh ws-ticket via authenticated session cookie" : "Minted fresh ws-ticket via Bearer auth")")
        return ticket
    }

    /// Rotate the stored access token when it is missing or about to expire,
    /// using the stored refresh token against /auth/native/refresh. The
    /// gateway never refreshes bearer tokens for us -- the desktop (this app)
    /// owns the refresh token and must rotate before minting tickets, or an
    /// expired access token makes every reconnect fail with 401 and the app
    /// goes permanently offline while still appearing logged in.
    func refreshAccessTokenIfNeeded() async throws -> String {
        let now = Int(Date().timeIntervalSince1970)
        if let stored = await secureStore.retrieve(key: "nativeGatewayAccessToken"),
           let expiresRaw = await secureStore.retrieve(key: "nativeGatewayAccessTokenExpiresAt"),
           let expires = Int(expiresRaw),
           expires > now + 60 {
            // Still valid for at least a minute -- use as-is.
            return stored
        }
        return try await performTokenRefresh()
    }

    /// Force a bearer rotation, ignoring the cached `expiresAt` value. The
    /// connector delegates bearer verification to the gateway's
    /// `/api/auth/me`; if the cached `expiresAt` is ahead of reality (clock
    /// drift, server-side revocation, replay window expired), every
    /// connector-direct call returns 401 even though the local check
    /// considers the token valid. Used by registerPushToken and
    /// registerNativeWatch to recover from that 401 in a single retry.
    func clearLocalCredentials() async {
        await secureStore.delete(key: "nativeGatewayAccessToken")
        await secureStore.delete(key: "nativeGatewayRefreshToken")
        await secureStore.delete(key: "nativeGatewayAccessTokenExpiresAt")
        await secureStore.delete(key: "nativeGatewayBasicUsername")
        await secureStore.delete(key: "nativeGatewayBasicPassword")
    }

    func forceRefreshAccessToken() async throws -> String {
        await secureStore.delete(key: "nativeGatewayAccessTokenExpiresAt")
        return try await performTokenRefresh()
    }

    /// Shared refresh implementation: POST the stored refresh token to
    /// `/auth/native/refresh` and persist the rotated access + refresh +
    /// expiresAt. Throws `.notLoggedIn` when no refresh token is stored or
    /// the refresh itself fails (callers treat that as a fatal session-loss
    /// signal -- the keychain is wiped so the app falls back to onboarding).
    private func performTokenRefresh() async throws -> String {
        guard let refreshToken = await secureStore.retrieve(key: "nativeGatewayRefreshToken") else {
            throw NativeAuthError.notLoggedIn
        }
        var request = URLRequest(url: URL(string: "\(gatewayBaseURL)/auth/native/refresh")!)
        request.httpMethod = "POST"
        // LATENCY (build 36): refreshAccessTokenIfNeeded() runs FIRST inside
        // mintTicket(), before mintTicket's own (build 34) 8s-bounded POST.
        // This request used URLSession.shared's default 60s timeout with no
        // override -- on a dead/half-open network path (backgrounded app,
        // cell handoff) whenever the stored token was within 60s of expiry,
        // this alone could eat up to 60s BEFORE the already-bounded ticket
        // mint (8s) and connect-verify (5s) probes ever ran, reintroducing
        // most of the delay build 34 thought it had eliminated. Same 8s
        // bound as every other reconnect-path HTTP call in this file.
        request.timeoutInterval = 8
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["refresh_token": refreshToken, "provider": "nous"])

        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
                throw NativeAuthError.tokenRefreshFailed
            }
            let tokens = try JSONDecoder().decode(NativeTokenResponse.self, from: data)
            await secureStore.store(key: "nativeGatewayAccessToken", value: tokens.accessToken)
            await secureStore.store(key: "nativeGatewayRefreshToken", value: tokens.refreshToken)
            await secureStore.store(key: "nativeGatewayAccessTokenExpiresAt", value: String(tokens.expiresAt))
            logger.info("Rotated native gateway tokens via /auth/native/refresh")
            return tokens.accessToken
        } catch {
            // A dead/expired refresh token means the session is over. Clear
            // so the app falls back to onboarding instead of retrying forever.
            await secureStore.delete(key: "nativeGatewayAccessToken")
            await secureStore.delete(key: "nativeGatewayRefreshToken")
            await secureStore.delete(key: "nativeGatewayAccessTokenExpiresAt")
            throw NativeAuthError.notLoggedIn
        }
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
    case tokenRefreshFailed
    case invalidCredentials
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
        case .tokenRefreshFailed:
            return "Couldn't refresh the gateway session."
        case .invalidCredentials:
            return "The gateway username or password was not accepted."
        }
    }

}
