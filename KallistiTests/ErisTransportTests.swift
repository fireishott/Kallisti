import Foundation
import Testing
@testable import Kallisti

// MARK: - Pairing payload

@Suite("Eris pairing payload")
struct ErisPairingTests {

    @Test("a compact JSON payload decodes to a code and a base URL")
    func decodesTheGoodPayload() throws {
        let payload = #"{"code":"AB2CD3EF","relay":"http://192.168.1.20:7341"}"#
        let pairing = try ErisPairing(payload: payload)
        #expect(pairing.code == "AB2CD3EF")
        #expect(pairing.baseURL.absoluteString == "http://192.168.1.20:7341")
    }

    @Test("extra keys in the payload do not break the decode")
    func toleratesExtraKeys() throws {
        let payload = #"{"code":"AB2CD3EF","relay":"https://eris.example:7341","v":1}"#
        let pairing = try ErisPairing(payload: payload)
        #expect(pairing.code == "AB2CD3EF")
        #expect(pairing.baseURL.scheme == "https")
    }

    @Test("a payload that is not JSON is rejected")
    func rejectsNonJSON() {
        expectPairingError("not json at all", .notJSON)
        expectPairingError(#"{"code":"AB2CD3EF""#, .notJSON)
        expectPairingError("[\"code\",\"relay\"]", .notJSON)
        expectPairingError("", .notJSON)
    }

    @Test("a payload missing a key is rejected with that key's error")
    func rejectsMissingKeys() {
        expectPairingError(#"{"code":"AB2CD3EF"}"#, .missingRelay)
        expectPairingError(#"{"relay":"http://192.168.1.20:7341"}"#, .missingCode)
        expectPairingError(#"{"code":null,"relay":"http://192.168.1.20:7341"}"#, .missingCode)
        expectPairingError(#"{"code":"AB2CD3EF","relay":42}"#, .missingRelay)
    }

    @Test("a relay that is not an http(s) address is rejected")
    func rejectsUnusableRelay() {
        expectPairingError(#"{"code":"AB2CD3EF","relay":""}"#, .missingRelay)
        expectPairingError(#"{"code":"AB2CD3EF","relay":"ftp://host:7341"}"#, .invalidRelay("ftp://host:7341"))
        expectPairingError(#"{"code":"AB2CD3EF","relay":"not a url"}"#, .invalidRelay("not a url"))
    }

    @Test("an empty code is rejected")
    func rejectsEmptyCode() {
        expectPairingError(#"{"code":"   ","relay":"http://192.168.1.20:7341"}"#, .invalidCode)
    }

    @Test("a manually typed base URL and code produce the same value")
    func manualEntry() throws {
        let pairing = try ErisPairing(baseURLString: "  http://192.168.1.20:7341  ", code: " AB2CD3EF ")
        #expect(pairing.code == "AB2CD3EF")
        #expect(pairing.baseURL.host == "192.168.1.20")
        #expect(pairing.baseURL.port == 7341)
    }

    @Test("manual entry rejects an unusable base URL")
    func manualEntryRejectsBadHost() {
        #expect(throws: ErisPairingError.self) {
            _ = try ErisPairing(baseURLString: "192.168.1.20:7341", code: "AB2CD3EF")
        }
    }

    /// Assert one payload is rejected with one specific typed error.
    private func expectPairingError(_ payload: String, _ expected: ErisPairingError) {
        do {
            let pairing = try ErisPairing(payload: payload)
            Issue.record("expected \(expected) for \(payload), got \(pairing)")
        } catch let error as ErisPairingError {
            #expect(error == expected)
        } catch {
            Issue.record("expected \(expected) for \(payload), got \(error)")
        }
    }
}

// MARK: - SSE frames

@Suite("Eris SSE decoding")
struct ErisSSETests {

    @Test("several frames in one chunk decode in order, terminal last")
    func severalFrames() throws {
        // Bytes on the wire, spelled out: `id:`/`event:`/`data:` lines with a
        // blank line after each frame.
        let stream = "id: 1\nevent: delta\ndata: {\"text\":\"Hel\"}\n\n"
            + "id: 2\nevent: delta\ndata: {\"text\":\"lo\"}\n\n"
            + "id: 3\nevent: done\ndata: {\"text\":\"Hello\",\"usage\":{\"input\":{\"state\":\"reported\",\"tokens\":7},\"output\":{\"state\":\"reported\",\"tokens\":2},\"cache_read\":{\"state\":\"provider_unavailable\",\"reason\":\"fields_absent\"}}}\n\n"

        var decoder = ErisSSEDecoder()
        let frames = try decoder.consume(Data(stream.utf8))

        guard frames.count == 3 else {
            Issue.record("expected 3 frames, got \(frames.count): \(frames)")
            return
        }
        #expect(frames[0] == .delta(text: "Hel"))
        #expect(frames[1] == .delta(text: "lo"))
        guard case .done(let text, let usage) = frames[2] else {
            Issue.record("expected a done frame, got \(frames[2])")
            return
        }
        #expect(text == "Hello")
        #expect(usage?.inputTokens == 7)
        #expect(usage?.outputTokens == 2)
        // An unreported cache count is absent, never zero.
        #expect(usage?.cacheReadTokens == nil)
        #expect(frames[2].isTerminal)
        #expect(!frames[0].isTerminal)
    }

    @Test("a frame split across chunks decodes once, after its terminator")
    func splitFrameAcrossChunks() throws {
        var decoder = ErisSSEDecoder()

        // First chunk ends mid-field-name.
        let partialName = try decoder.consume(Data("event: del".utf8))
        #expect(partialName.isEmpty)
        // Second chunk finishes the event line and the data line, no terminator.
        let partialBody = try decoder.consume(Data("ta\ndata: {\"text\":\"hi\"}".utf8))
        #expect(partialBody.isEmpty)
        // Third chunk is the blank line that dispatches.
        let frames = try decoder.consume(Data("\n\n".utf8))
        #expect(frames == [.delta(text: "hi")])
    }

    @Test("a byte-at-a-time stream decodes the same frames")
    func byteAtATime() throws {
        let stream = "event: delta\ndata: {\"text\":\"a\"}\n\nevent: cancelled\ndata: {\"reason\":\"cancel\"}\n\n"
        var decoder = ErisSSEDecoder()
        var frames: [ErisFrame] = []
        for byte in Data(stream.utf8) {
            frames.append(contentsOf: try decoder.consume(Data([byte])))
        }
        #expect(frames == [.delta(text: "a"), .cancelled(reason: "cancel")])
    }

    @Test("an error frame carries the daemon's code and message and ends the stream")
    func errorFrame() throws {
        let stream = "id: 4\nevent: error\ndata: {\"code\":\"provider_unavailable\",\"message\":\"upstream 503\"}\n\n"
        var decoder = ErisSSEDecoder()
        let frames = try decoder.consume(Data(stream.utf8))

        guard let frame = frames.first, frames.count == 1 else {
            Issue.record("expected exactly 1 frame, got \(frames.count): \(frames)")
            return
        }
        #expect(frame.isTerminal)
        #expect(frame.text == nil)
        guard case .error(let error) = frame else {
            Issue.record("expected an error frame, got \(frame)")
            return
        }
        #expect(error.code == "provider_unavailable")
        #expect(error.message == "upstream 503")
        #expect(error.status == nil)
    }

    @Test("keep-alive comments dispatch nothing")
    func keepAliveComment() throws {
        var decoder = ErisSSEDecoder()
        let frames = try decoder.consume(Data(": keep-alive\n\n".utf8))
        #expect(frames.isEmpty)
    }

    @Test("CRLF line endings are handled")
    func crlfEndings() throws {
        var decoder = ErisSSEDecoder()
        let frames = try decoder.consume(Data("event: delta\r\ndata: {\"text\":\"ok\"}\r\n\r\n".utf8))
        #expect(frames == [.delta(text: "ok")])
    }

    @Test("a trailing frame with no terminator is flushed by finish()")
    func finishFlushesTrailingFrame() throws {
        var decoder = ErisSSEDecoder()
        let pending = try decoder.consume(Data("event: delta\ndata: {\"text\":\"tail\"}".utf8))
        #expect(pending.isEmpty)
        let flushed = try decoder.finish()
        #expect(flushed == [.delta(text: "tail")])
    }

    @Test("an event name this client does not know is skipped, not fatal")
    func unknownEventNameIsSkipped() throws {
        let stream = "event: reasoning\ndata: {\"text\":\"hidden\"}\n\nevent: delta\ndata: {\"text\":\"shown\"}\n\n"
        var decoder = ErisSSEDecoder()
        let frames = try decoder.consume(Data(stream.utf8))
        #expect(frames == [.delta(text: "shown")])
    }

    @Test("a data payload that is not JSON throws a typed stream error")
    func malformedDataPayload() {
        var decoder = ErisSSEDecoder()
        do {
            _ = try decoder.consume(Data("event: delta\ndata: {oops\n\n".utf8))
            Issue.record("expected a stream error")
        } catch let error as ErisError {
            #expect(error.message?.isEmpty == false)
        } catch {
            Issue.record("expected an ErisError, got \(error)")
        }
    }
}

// MARK: - Routes

@Suite("Eris route URLs")
struct ErisRouteTests {
    private let base = URL(string: "http://192.168.1.20:7341")!

    @Test("every route path is the daemon's /v1 literal")
    func routePaths() {
        #expect(ErisRoute.pair.path == "/v1/pair")
        #expect(ErisRoute.tokenRefresh.path == "/v1/token/refresh")
        #expect(ErisRoute.turns.path == "/v1/turns")
        #expect(ErisRoute.events(turnID: "job-1").path == "/v1/turns/job-1/events")
        #expect(ErisRoute.cancel(turnID: "job-1").path == "/v1/turns/job-1/cancel")
        #expect(ErisRoute.session(sessionID: "s-1").path == "/v1/sessions/s-1")
    }

    @Test("methods match the daemon's router")
    func routeMethods() {
        #expect(ErisRoute.pair.method == "POST")
        #expect(ErisRoute.tokenRefresh.method == "POST")
        #expect(ErisRoute.turns.method == "POST")
        #expect(ErisRoute.cancel(turnID: "job-1").method == "POST")
        #expect(ErisRoute.events(turnID: "job-1").method == "GET")
        #expect(ErisRoute.session(sessionID: "s-1").method == "GET")
    }

    @Test("absolute URLs join the payload's base and the route path")
    func absoluteURLs() throws {
        let pair = try ErisClient.url(baseURL: base, route: .pair)
        let refresh = try ErisClient.url(baseURL: base, route: .tokenRefresh)
        let turns = try ErisClient.url(baseURL: base, route: .turns)
        let events = try ErisClient.url(baseURL: base, route: .events(turnID: "job-1"))
        let cancel = try ErisClient.url(baseURL: base, route: .cancel(turnID: "job-1"))
        let session = try ErisClient.url(baseURL: base, route: .session(sessionID: "s-1"))

        #expect(pair.absoluteString == "http://192.168.1.20:7341/v1/pair")
        #expect(refresh.absoluteString == "http://192.168.1.20:7341/v1/token/refresh")
        #expect(turns.absoluteString == "http://192.168.1.20:7341/v1/turns")
        #expect(events.absoluteString == "http://192.168.1.20:7341/v1/turns/job-1/events")
        #expect(cancel.absoluteString == "http://192.168.1.20:7341/v1/turns/job-1/cancel")
        #expect(session.absoluteString == "http://192.168.1.20:7341/v1/sessions/s-1")
    }

    @Test("a trailing slash on the base URL does not double up")
    func trailingSlashBase() throws {
        let base = URL(string: "http://192.168.1.20:7341/")!
        let turns = try ErisClient.url(baseURL: base, route: .turns)
        #expect(turns.absoluteString == "http://192.168.1.20:7341/v1/turns")
    }

    // Regression: a base URL that already ends in `/v1` (what
    // `RelayConfiguration.normalizeBaseURL` hands the client) used to be joined
    // with a route path that also starts with `/v1`, producing `/v1/v1/pair`
    // and a 404 on every route.
    @Test("a base URL that already ends in /v1 joins exactly one /v1")
    func normalizedBaseDoesNotDoubleUp() throws {
        let base = URL(string: "https://fih-ai-host.example.ts.net/v1")!
        let pair = try ErisClient.url(baseURL: base, route: .pair)
        let refresh = try ErisClient.url(baseURL: base, route: .tokenRefresh)
        let turns = try ErisClient.url(baseURL: base, route: .turns)
        let events = try ErisClient.url(baseURL: base, route: .events(turnID: "job-1"))
        #expect(pair.absoluteString == "https://fih-ai-host.example.ts.net/v1/pair")
        #expect(refresh.absoluteString == "https://fih-ai-host.example.ts.net/v1/token/refresh")
        #expect(turns.absoluteString == "https://fih-ai-host.example.ts.net/v1/turns")
        #expect(events.absoluteString == "https://fih-ai-host.example.ts.net/v1/turns/job-1/events")
    }

    @Test("a base URL ending in /v1/ also joins exactly one /v1")
    func normalizedBaseWithTrailingSlashDoesNotDoubleUp() throws {
        let base = URL(string: "https://fih-ai-host.example.ts.net/v1/")!
        let pair = try ErisClient.url(baseURL: base, route: .pair)
        #expect(pair.absoluteString == "https://fih-ai-host.example.ts.net/v1/pair")
    }

    @Test("a base URL with no /v1 still gets the route's own /v1")
    func bareBaseStillGetsV1() throws {
        let base = URL(string: "https://fih-ai-host.example.ts.net")!
        let pair = try ErisClient.url(baseURL: base, route: .pair)
        #expect(pair.absoluteString == "https://fih-ai-host.example.ts.net/v1/pair")
    }

    @Test("an id cannot escape its path segment")
    func idsAreEscaped() {
        #expect(ErisRoute.events(turnID: "a/b?c").path == "/v1/turns/a%2Fb%3Fc/events")
        #expect(ErisRoute.session(sessionID: "s 1").path == "/v1/sessions/s%201")
    }

    @Test("form bodies are sorted and escaped")
    func formEncoding() {
        let encoded = ErisClient.formEncode(["code": "AB2CD3EF", "device_name": "iPhone 16"])
        #expect(encoded == "code=AB2CD3EF&device_name=iPhone%2016")
    }
}

// MARK: - Transport

/// A token store that never touches the keychain.
@MainActor
final class InMemoryErisTokenStore: ErisTokenStoring {
    private(set) var stored: ErisStoredTokens?

    init(stored: ErisStoredTokens? = nil) {
        self.stored = stored
    }

    func load() async throws -> ErisStoredTokens? {
        stored
    }

    func save(_ tokens: ErisStoredTokens) async throws {
        stored = tokens
    }

    func clear() async {
        stored = nil
    }
}

/// Stubs every request, so these tests never open a socket.
final class ErisStubURLProtocol: URLProtocol, @unchecked Sendable {
    /// Set per test; the suite is serialized so there is exactly one at a time.
    nonisolated(unsafe) static var handler: (@Sendable (URLRequest) throws -> (Int, Data))?

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let handler = Self.handler else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        do {
            let (status, data) = try handler(request)
            let url = request.url ?? URL(string: "http://stub.invalid")!
            guard let response = HTTPURLResponse(
                url: url,
                statusCode: status,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
            ) else {
                client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
                return
            }
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    override func stopLoading() {}
}

/// Counts turn submissions so the 401-then-refresh path is deterministic.
final class ErisStubTurnCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0

    func next() -> Int {
        lock.lock()
        defer { lock.unlock() }
        count += 1
        return count
    }
}

@MainActor
@Suite("Eris client transport", .serialized)
struct ErisClientTests {
    private static let base = URL(string: "http://192.168.1.20:7341")!

    private static func stubSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ErisStubURLProtocol.self]
        return URLSession(configuration: configuration)
    }

    private static func storedPair(
        accessToken: String = "access-1",
        refreshToken: String? = "refresh-1"
    ) -> ErisStoredTokens {
        ErisStoredTokens(
            baseURL: base,
            tokens: ErisTokenPair(
                accessToken: accessToken,
                refreshToken: refreshToken,
                deviceID: "device-1",
                scopes: ["chat", "sessions"],
                expiresAt: nil
            )
        )
    }

    @Test("pair posts the form body and stores the minted pair")
    func pairStoresTokens() async throws {
        let store = InMemoryErisTokenStore()
        ErisStubURLProtocol.handler = { request in
            #expect(request.httpMethod == "POST")
            #expect(request.url?.path == "/v1/pair")
            let body = #"{"ok":true,"access_token":"access-1","refresh_token":"refresh-1","device_id":"device-1","scopes":["chat","sessions"],"expires_at":"2026-10-11T12:00:00Z"}"#
            return (201, Data(body.utf8))
        }
        defer { ErisStubURLProtocol.handler = nil }

        let client = ErisClient(baseURL: Self.base, tokenStore: store, session: Self.stubSession())
        let pair = try await client.pair(code: "AB2CD3EF", deviceName: "iPhone 16")

        #expect(pair.accessToken == "access-1")
        #expect(pair.refreshToken == "refresh-1")
        #expect(pair.deviceID == "device-1")
        #expect(pair.scopes == ["chat", "sessions"])
        #expect(pair.expiresAt != nil)
        #expect(store.stored?.accessToken == "access-1")
        #expect(store.stored?.baseURL == Self.base)
    }

    @Test("a 401 on a turn refreshes once and retries")
    func unauthorizedRefreshesOnce() async throws {
        let store = InMemoryErisTokenStore(stored: Self.storedPair())
        let turns = ErisStubTurnCounter()
        ErisStubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/token/refresh":
                let body = #"{"ok":true,"access_token":"access-2","expires_at":"2026-10-11T13:00:00Z"}"#
                return (200, Data(body.utf8))
            case "/v1/turns":
                if turns.next() == 1 {
                    let body = #"{"ok":false,"code":"token_invalid","message":"the access token does not verify"}"#
                    return (401, Data(body.utf8))
                }
                let body = #"{"ok":true,"job_id":"job-1","session_id":"session-1","state":"queued"}"#
                return (202, Data(body.utf8))
            default:
                return (404, Data(#"{"ok":false,"code":"not_found","message":"nope"}"#.utf8))
            }
        }
        defer { ErisStubURLProtocol.handler = nil }

        let client = ErisClient(baseURL: Self.base, tokenStore: store, session: Self.stubSession())
        let turn = try await client.createTurn(text: "hello")

        #expect(turn.jobID == "job-1")
        #expect(turn.sessionID == "session-1")
        #expect(turn.state == "queued")
        // The refresh kept the refresh token it already had.
        #expect(store.stored?.accessToken == "access-2")
        #expect(store.stored?.refreshToken == "refresh-1")
    }

    @Test("a refusal surfaces the daemon's code")
    func serverErrorCarriesCode() async throws {
        let store = InMemoryErisTokenStore(stored: Self.storedPair())
        ErisStubURLProtocol.handler = { _ in
            let body = #"{"ok":false,"code":"turn_not_cancellable","message":"the turn already finished"}"#
            return (409, Data(body.utf8))
        }
        defer { ErisStubURLProtocol.handler = nil }

        let client = ErisClient(baseURL: Self.base, tokenStore: store, session: Self.stubSession())
        do {
            try await client.cancel(turnID: "job-1")
            Issue.record("expected a refusal")
        } catch let error as ErisError {
            #expect(error.code == "turn_not_cancellable")
            #expect(error.status == 409)
        } catch {
            Issue.record("expected an ErisError, got \(error)")
        }
    }

    @Test("a route that needs a bearer fails fast with no token stored")
    func missingTokenIsTyped() async throws {
        let store = InMemoryErisTokenStore()
        ErisStubURLProtocol.handler = { _ in
            Issue.record("no request should leave the client without a token")
            return (500, Data())
        }
        defer { ErisStubURLProtocol.handler = nil }

        let client = ErisClient(baseURL: Self.base, tokenStore: store, session: Self.stubSession())
        do {
            _ = try await client.createTurn(text: "hello")
            Issue.record("expected a missingToken error")
        } catch let error as ErisError {
            #expect(error == .missingToken)
        } catch {
            Issue.record("expected an ErisError, got \(error)")
        }
    }
}
