import Foundation
import os

/// Coordinates authentication for the native gateway WebSocket.
///
/// Supports two flows:
/// - **Password-login**: POST /auth/password-login → cookie → POST /api/auth/ws-ticket → ticket
/// - **OAuth/PKCE** (future): RFC 8252 broker flow → token → ws-ticket
///
/// The ticket is appended as a query parameter to the WS URL.
enum NativeAuthCoordinator {
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "NativeAuth")

    struct Credentials: Sendable {
        let username: String
        let password: String
        let provider: String
    }

    enum AuthError: Error, LocalizedError {
        case invalidBaseURL
        case loginFailed(statusCode: Int, body: String)
        case noCookieFromLogin
        case ticketFailed(statusCode: Int, body: String)
        case noTicketInResponse
        case encodingFailed

        var errorDescription: String? {
            switch self {
            case .invalidBaseURL: return "Invalid gateway base URL"
            case .loginFailed(let code, _): return "Password login failed (HTTP \(code))"
            case .noCookieFromLogin: return "No session cookie returned from login"
            case .ticketFailed(let code, _): return "Ws-ticket request failed (HTTP \(code))"
            case .noTicketInResponse: return "No ticket in ws-ticket response"
            case .encodingFailed: return "Failed to encode request body"
            }
        }
    }

    /// Get a ws-ticket URL for the native gateway using password-login flow.
    ///
    /// - Parameters:
    ///   - baseURL: The gateway's HTTP base URL (e.g., `http://192.168.10.118:9119` or `https://hermes-relay.fihonline.net`)
    ///   - credentials: Username/password/provider for login
    ///   - session: URLSession to use (default .shared)
    /// - Returns: A URL suitable for `URLSessionWebSocketTransport.connect(url:)`
    static func authenticatedWSURL(
        baseURL: String,
        credentials: Credentials,
        session: URLSession = .shared
    ) async throws -> URL {
        guard let base = URL(string: baseURL) else {
            throw AuthError.invalidBaseURL
        }

        // Step 1: Password login → cookie
        let cookie = try await passwordLogin(base: base, credentials: credentials, session: session)
        logger.info("Password login succeeded, got cookie")

        // Step 2: Get ws-ticket
        let ticket = try await requestTicket(base: base, cookie: cookie, session: session)
        logger.info("Got ws-ticket")

        // Step 3: Build WS URL with ticket
        var components = URLComponents(url: base.appendingPathComponent("api/ws"), resolvingAgainstBaseURL: false)!
        components.scheme = base.scheme == "https" ? "wss" : "ws"
        components.queryItems = [URLQueryItem(name: "ticket", value: ticket)]

        guard let wsURL = components.url else {
            throw AuthError.invalidBaseURL
        }
        return wsURL
    }

    // MARK: - Private

    private static func passwordLogin(
        base: URL,
        credentials: Credentials,
        session: URLSession
    ) async throws -> String {
        let url = base.appendingPathComponent("auth/password-login")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let body: [String: String] = [
            "provider": credentials.provider,
            "username": credentials.username,
            "password": credentials.password,
        ]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await session.data(for: request)
        let httpResponse = response as! HTTPURLResponse

        guard httpResponse.statusCode == 200 else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw AuthError.loginFailed(statusCode: httpResponse.statusCode, body: body)
        }

        // Extract cookie from response
        guard let headerFields = httpResponse.allHeaderFields as? [String: String],
              let setCookie = headerFields["Set-Cookie"] ?? headerFields["set-cookie"] else {
            throw AuthError.noCookieFromLogin
        }

        // Cookie format: "name=value; path=/; ..."
        let cookie = setCookie.split(separator: ";").first.map(String.init) ?? setCookie
        return cookie
    }

    private static func requestTicket(
        base: URL,
        cookie: String,
        session: URLSession
    ) async throws -> String {
        let url = base.appendingPathComponent("api/auth/ws-ticket")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue(cookie, forHTTPHeaderField: "Cookie")

        let (data, response) = try await session.data(for: request)
        let httpResponse = response as! HTTPURLResponse

        guard httpResponse.statusCode == 200 else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw AuthError.ticketFailed(statusCode: httpResponse.statusCode, body: body)
        }

        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let ticket = json["ticket"] as? String else {
            throw AuthError.noTicketInResponse
        }

        return ticket
    }
}
