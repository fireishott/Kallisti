import Foundation
import CryptoKit
import SafariServices
import UIKit
import Security
import os

@MainActor
final class NativeAuthCoordinator: NSObject, SFSafariViewControllerDelegate {
    private let logger = Logger(subsystem: "net.fihonline.kallisti", category: "NativeAuth")
    private let host: String
    private let port: Int
    private let session: URLSession
    private let secureStore: any SecureStoreProtocol
    private var activeSafari: SFSafariViewController?
    private var activeListener: LoopbackCallbackListener?

    init(host: String, port: Int, session: URLSession = .shared, secureStore: any SecureStoreProtocol) {
        self.host = host
        self.port = port
        self.session = session
        self.secureStore = secureStore
    }

    // MARK: - Public

    func startLogin(presentingFrom viewController: UIViewController) async throws {
        let verifier = Self.randomPKCEVerifier()
        let challenge = Self.s256Challenge(for: verifier)
        let state = Self.randomState()

        let listener = LoopbackCallbackListener()
        activeListener = listener
        try await listener.start()

        var components = URLComponents(string: "http://\(host):\(port)/auth/native/authorize")!
        components.queryItems = [
            URLQueryItem(name: "code_challenge", value: challenge),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "redirect_uri", value: "http://127.0.0.1:\(listener.port)/callback"),
            URLQueryItem(name: "state", value: state),
        ]

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
        logger.info("Callback received, state matched, exchanging code for tokens")
        try await exchangeCode(callback.code, verifier: verifier)
    }

    nonisolated func safariViewControllerDidFinish(_ controller: SFSafariViewController) {
        Task { @MainActor in
            activeListener?.stop()
            activeListener = nil
        }
    }

    func exchangeCode(_ code: String, verifier: String) async throws {
        var request = URLRequest(url: URL(string: "http://\(host):\(port)/auth/native/token")!)
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

    func mintTicket() async throws -> String {
        guard let accessToken = await secureStore.retrieve(key: "nativeGatewayAccessToken") else {
            throw NativeAuthError.notLoggedIn
        }
        var request = URLRequest(url: URL(string: "http://\(host):\(port)/api/auth/ws-ticket")!)
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
    case callbackParseFailed
    case stateMismatch
    case tokenExchangeFailed
    case notLoggedIn
    case ticketMintFailed
}
