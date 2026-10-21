import Foundation
import Testing
@testable import Kallisti

// MARK: - Stub transport

/// Routes stubbed responses for the Eris app surface.
///
/// A `.deferred` route answers the response head immediately and holds the body
/// until the test signals, which is what makes "Stop while streaming" testable
/// without racing a real socket.
final class ErisSurfaceStub: URLProtocol, @unchecked Sendable {
    /// What the stub should do for one request.
    enum Behavior {
        /// Answer with a status and a complete body.
        case immediate(Int, Data)
        /// Send the status line now, the body when the gate opens.
        case deferred(Int, Data, gate: DispatchSemaphore)
    }

    /// Set per test. The suites are serialized, so there is one at a time.
    /// `nonisolated(unsafe)` because `URLProtocol` calls back on its own thread;
    /// the lock below is what guards the recording, not actor isolation.
    nonisolated(unsafe) static var route: ((URLRequest) -> Behavior)?

    /// Paths the stub was asked for, in order.
    nonisolated(unsafe) private static var paths: [String] = []
    /// Guards `paths`, which is appended from protocol threads.
    private static let pathsLock = NSLock()

    static func reset() {
        route = nil
        pathsLock.lock()
        paths.removeAll()
        pathsLock.unlock()
    }

    /// Every path the stub was asked for, in order.
    static func pathsSnapshot() -> [String] {
        pathsLock.lock()
        defer { pathsLock.unlock() }
        return paths
    }

    private static func record(_ path: String) {
        pathsLock.lock()
        paths.append(path)
        pathsLock.unlock()
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let request = self.request
        Self.record(request.url?.path ?? "")
        guard let route = Self.route else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        let url = request.url ?? URL(string: "http://stub.invalid") ?? URL(fileURLWithPath: "/")
        switch route(request) {
        case .immediate(let status, let data):
            deliver(status: status, data: data, url: url)
        case .deferred(let status, let data, let gate):
            guard let response = Self.response(status: status, url: url) else {
                client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
                return
            }
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            DispatchQueue.global().async { [self] in
                _ = gate.wait(timeout: .now() + 5)
                if !data.isEmpty {
                    client?.urlProtocol(self, didLoad: data)
                }
                client?.urlProtocolDidFinishLoading(self)
            }
        }
    }

    override func stopLoading() {}

    private func deliver(status: Int, data: Data, url: URL) {
        guard let response = Self.response(status: status, url: url) else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        if !data.isEmpty {
            client?.urlProtocol(self, didLoad: data)
        }
        client?.urlProtocolDidFinishLoading(self)
    }

    private static func response(status: Int, url: URL) -> HTTPURLResponse? {
        HTTPURLResponse(
            url: url,
            statusCode: status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "text/event-stream"]
        )
    }
}

// MARK: - Helpers

/// Build one SSE body from `(event, data)` pairs.
private func sse(_ frames: [(String, String)]) -> Data {
    var text = ""
    for frame in frames {
        text += "event: \(frame.0)\ndata: \(frame.1)\n\n"
    }
    return Data(text.utf8)
}

/// A stub-backed session for the transport.
private func stubbedSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [ErisSurfaceStub.self]
    return URLSession(configuration: configuration)
}

/// An isolated defaults domain, so tests never touch the app's real preferences.
private func scratchDefaults() -> UserDefaults {
    let name = "kallisti.eris.tests.\(UUID().uuidString)"
    guard let defaults = UserDefaults(suiteName: name) else {
        return UserDefaults.standard
    }
    defaults.removePersistentDomain(forName: name)
    return defaults
}

private let testBase = URL(string: "http://192.168.1.20:7341") ?? URL(fileURLWithPath: "/")

private func storedPair(
    baseURL: URL = testBase,
    accessToken: String = "access-1",
    refreshToken: String? = "refresh-1"
) -> ErisStoredTokens {
    ErisStoredTokens(
        baseURL: baseURL,
        tokens: ErisTokenPair(
            accessToken: accessToken,
            refreshToken: refreshToken,
            deviceID: "device-1",
            scopes: ["chat"],
            expiresAt: nil
        )
    )
}

private let pairedBody = #"{"ok":true,"access_token":"access-1","refresh_token":"refresh-1","device_id":"device-1","scopes":["chat"],"expires_at":"2026-10-11T12:00:00Z"}"#

private func unauthorizedBody(_ code: String) -> Data {
    Data(#"{"ok":false,"code":"\#(code)","message":"no"}"#.utf8)
}

/// Wait for a condition on the main actor, without a fixed sleep.
@MainActor
private func waitUntil(
    _ condition: () -> Bool,
    attempts: Int = 400
) async -> Bool {
    for _ in 0..<attempts {
        if condition() { return true }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return condition()
}

// MARK: - Daemon store

/// One serialized parent, so the two suites that share a stub registry can never
/// interleave. `.serialized` applies to child suites as well as to their tests.
@Suite(.serialized)
@MainActor
struct ErisStubBackedSuites {
    @Suite("Eris daemon store")
    @MainActor
    struct ErisDaemonStoreTests {
    @Test("pairing from typed fields stores the pair and lands on chat")
    func pairFromFields() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { request in
            #expect(request.url?.path == "/v1/pair")
            #expect(request.httpMethod == "POST")
            return .immediate(201, Data(pairedBody.utf8))
        }
        let defaults = scratchDefaults()
        let tokens = InMemoryErisTokenStore()
        let store = ErisDaemonStore(
            tokenStore: tokens,
            defaults: defaults,
            session: stubbedSession()
        )
        store.addressText = "http://192.168.1.20:7341"
        store.codeText = "AB2CD3EF"

        await store.pairFromFields()

        #expect(store.pairedBaseURL?.absoluteString == "http://192.168.1.20:7341")
        #expect(store.chat != nil)
        #expect(store.failure == nil)
        #expect(tokens.stored?.accessToken == "access-1")
        #expect(defaults.string(forKey: "eris.daemon.baseURL") == "http://192.168.1.20:7341")
    }

    @Test("a bare host:port is treated as http")
    func bareAddressGainsScheme() async {
        defer { ErisSurfaceStub.reset() }
        var seenHost: String?
        var seenPort: Int?
        ErisSurfaceStub.route = { request in
            seenHost = request.url?.host
            seenPort = request.url?.port
            return .immediate(201, Data(pairedBody.utf8))
        }
        let store = ErisDaemonStore(
            tokenStore: InMemoryErisTokenStore(),
            defaults: scratchDefaults(),
            session: stubbedSession()
        )
        store.addressText = " 192.168.1.20:7341 "
        store.codeText = "AB2CD3EF"

        await store.pairFromFields()

        #expect(seenHost == "192.168.1.20")
        #expect(seenPort == 7341)
        #expect(store.pairedBaseURL?.scheme == "http")
    }

    @Test("an unbindable address is rejected before any request")
    func rejectsBadAddress() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { _ in
            Issue.record("no request should be sent for an unusable address")
            return .immediate(500, Data())
        }
        let store = ErisDaemonStore(
            tokenStore: InMemoryErisTokenStore(),
            defaults: scratchDefaults(),
            session: stubbedSession()
        )
        store.addressText = "ftp://host:7341"
        store.codeText = "AB2CD3EF"

        await store.pairFromFields()

        #expect(store.failure != nil)
        #expect(store.pairedBaseURL == nil)
        #expect(ErisSurfaceStub.pathsSnapshot().isEmpty)
    }

    @Test("a refusal surfaces the daemon's typed code")
    func pairingRefusalKeepsCode() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { _ in
            .immediate(401, unauthorizedBody("pairing_code_invalid"))
        }
        let store = ErisDaemonStore(
            tokenStore: InMemoryErisTokenStore(),
            defaults: scratchDefaults(),
            session: stubbedSession()
        )
        store.addressText = "http://192.168.1.20:7341"
        store.codeText = "nope"

        await store.pairFromFields()

        #expect(store.pairedBaseURL == nil)
        #expect(store.failureCode == "pairing_code_invalid")
        #expect(store.failure?.contains("401") == true)
    }

    @Test("a QR payload that is not a pairing payload is reported, not sent")
    func rejectsBadPayload() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { _ in
            Issue.record("an undecodable payload must not reach the network")
            return .immediate(500, Data())
        }
        let store = ErisDaemonStore(
            tokenStore: InMemoryErisTokenStore(),
            defaults: scratchDefaults(),
            session: stubbedSession()
        )

        await store.pair(payload: "not a pairing payload")

        #expect(store.failure != nil)
        #expect(store.pairedBaseURL == nil)
        #expect(ErisSurfaceStub.pathsSnapshot().isEmpty)
    }

    @Test("a stored pair that still verifies goes straight to chat")
    func restoreVerifiesWithSession() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { request in
            #expect(request.url?.path == "/v1/sessions/session-1")
            #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer access-1")
            return .immediate(
                200,
                Data(#"{"session_id":"session-1","state":"active","turns":[]}"#.utf8)
            )
        }
        let defaults = scratchDefaults()
        defaults.set("session-1", forKey: "eris.daemon.lastSessionID")
        let store = ErisDaemonStore(
            tokenStore: InMemoryErisTokenStore(stored: storedPair()),
            defaults: defaults,
            session: stubbedSession()
        )

        await store.start()

        #expect(store.pairedBaseURL?.absoluteString == "http://192.168.1.20:7341")
        #expect(store.chat != nil)
        #expect(store.failure == nil)
        // A restore must never queue a turn to prove a token works.
        #expect(!ErisSurfaceStub.pathsSnapshot().contains("/v1/turns"))
    }

    @Test("an expired pair is dropped and asks for a new code")
    func expiredPairIsDropped() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { request in
            if request.url?.path == "/v1/token/refresh" {
                return .immediate(401, unauthorizedBody("refresh_token_invalid"))
            }
            return .immediate(401, unauthorizedBody("access_token_invalid"))
        }
        let defaults = scratchDefaults()
        defaults.set("session-1", forKey: "eris.daemon.lastSessionID")
        let tokens = InMemoryErisTokenStore(stored: storedPair())
        let store = ErisDaemonStore(
            tokenStore: tokens,
            defaults: defaults,
            session: stubbedSession()
        )

        await store.start()

        #expect(store.pairedBaseURL == nil)
        #expect(store.chat == nil)
        #expect(tokens.stored == nil)
        #expect(defaults.string(forKey: "eris.daemon.lastSessionID") == nil)
        #expect(store.failure?.contains("expired") == true)
    }

    @Test("an unreachable daemon keeps the pair and says why")
    func unreachableDaemonKeepsPair() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { _ in
            .immediate(503, Data("daemon starting".utf8))
        }
        let store = ErisDaemonStore(
            tokenStore: InMemoryErisTokenStore(stored: storedPair()),
            defaults: scratchDefaults(),
            session: stubbedSession()
        )

        await store.start()

        #expect(store.pairedBaseURL != nil)
        #expect(store.failure == nil)
        #expect(store.notice?.contains("503") == true)
    }

    @Test("forget clears the pair, the address and the conversation")
    func forgetClearsEverything() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { _ in .immediate(200, Data()) }
        let defaults = scratchDefaults()
        defaults.set("http://192.168.1.20:7341", forKey: "eris.daemon.baseURL")
        let tokens = InMemoryErisTokenStore(stored: storedPair())
        let store = ErisDaemonStore(
            tokenStore: tokens,
            defaults: defaults,
            session: stubbedSession()
        )
        await store.start()

        await store.forget()

        #expect(store.phase == .unpaired)
        #expect(store.chat == nil)
        #expect(tokens.stored == nil)
        #expect(store.addressText.isEmpty)
        #expect(defaults.string(forKey: "eris.daemon.baseURL") == nil)
    }
}

// MARK: - Chat streaming

    @Suite("Eris chat streaming")
    @MainActor
    struct ErisChatStreamingTests {
    private func makeModel() -> ErisChatViewModel {
        let tokens = InMemoryErisTokenStore(stored: storedPair())
        let client = ErisClient(baseURL: testBase, tokenStore: tokens, session: stubbedSession())
        return ErisChatViewModel(client: client)
    }

    @Test("a turn streams into an answer row and reports usage")
    func streamsAnswer() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { request in
            switch request.url?.path {
            case "/v1/turns":
                return .immediate(
                    202,
                    Data(#"{"job_id":"job-1","session_id":"session-1","state":"queued"}"#.utf8)
                )
            case "/v1/turns/job-1/events":
                return .immediate(200, sse([
                    ("delta", #"{"text":"Hel"}"#),
                    ("delta", #"{"text":"lo\n"}"#),
                    ("done", #"{"text":"Hello\n","usage":{"input":{"state":"reported","tokens":7},"output":{"state":"reported","tokens":3}}}"#)
                ]))
            default:
                return .immediate(404, unauthorizedBody("not_found"))
            }
        }
        var observedSession: String?
        let model = ErisChatViewModel(
            client: ErisClient(
                baseURL: testBase,
                tokenStore: InMemoryErisTokenStore(stored: storedPair()),
                session: stubbedSession()
            ),
            onSessionID: { observedSession = $0 }
        )

        await model.send("hi")

        #expect(model.isStreaming == false)
        #expect(model.items.first?.kind == .question)
        #expect(model.items.first?.text == "hi")
        let answers = model.items.filter { $0.kind == .answer }
        #expect(answers.count == 1)
        #expect(answers.first?.text == "Hello\n")
        #expect(model.usageLine == "in 7 · out 3")
        #expect(observedSession == "session-1")
    }

    @Test("reasoning and tool regions render as their own rows")
    func classifiesRegions() async {
        defer { ErisSurfaceStub.reset() }
        let body = sse([
            ("delta", #"{"text":"Here is the answer.\n"}"#),
            ("delta", #"{"text":"```thinking\nLooking at the log\n```\n"}"#),
            ("delta", #"{"text":"```tool\nread file app.log\n```\n"}"#),
            ("done", #"{"text":"ok"}"#)
        ])
        ErisSurfaceStub.route = { request in
            request.url?.path == "/v1/turns"
                ? .immediate(202, Data(#"{"job_id":"job-1","session_id":"session-1","state":"queued"}"#.utf8))
                : .immediate(200, body)
        }
        let model = makeModel()

        await model.send("what happened")

        let kinds = model.items.map(\.kind)
        #expect(kinds.contains(.answer))
        #expect(kinds.contains(.reasoning))
        #expect(kinds.contains(.toolRun))
        let reasoning = model.items.first { $0.kind == .reasoning }
        #expect(reasoning?.text.contains("Looking at the log") == true)
        // The fence markers themselves are never rendered as text.
        #expect(reasoning?.text.contains("```") == false)
        let tool = model.items.first { $0.kind == .toolRun }
        #expect(tool?.text.contains("read file app.log") == true)
    }

    @Test("stop cancels the turn and the cancelled frame settles the surface")
    func stopCancels() async {
        defer {
            ErisSurfaceStub.reset()
        }
        let gate = DispatchSemaphore(value: 0)
        let cancelled = sse([("cancelled", #"{"reason":"cancel"}"#)])
        ErisSurfaceStub.route = { request in
            switch request.url?.path {
            case "/v1/turns":
                return .immediate(
                    202,
                    Data(#"{"job_id":"job-1","session_id":"session-1","state":"queued"}"#.utf8)
                )
            case "/v1/turns/job-1/cancel":
                return .immediate(200, Data(#"{"ok":true}"#.utf8))
            default:
                return .deferred(200, cancelled, gate: gate)
            }
        }
        let model = makeModel()

        let turn = Task { await model.send("long one") }
        defer {
            gate.signal()
            turn.cancel()
        }
        #expect(await waitUntil { model.isStreaming })

        await model.stop()
        gate.signal()
        await turn.value

        #expect(ErisSurfaceStub.pathsSnapshot().contains("/v1/turns/job-1/cancel"))
        #expect(model.isStreaming == false)
        #expect(model.items.contains { $0.kind == .notice })
        #expect(model.banner?.contains("cancelled") == true)
    }

    @Test("an error frame becomes a typed failure row")
    func errorFrame() async {
        defer { ErisSurfaceStub.reset() }
        let body = sse([("error", #"{"code":"provider_unavailable","message":"no provider"}"#)])
        ErisSurfaceStub.route = { request in
            request.url?.path == "/v1/turns"
                ? .immediate(202, Data(#"{"job_id":"job-1","session_id":"session-1","state":"queued"}"#.utf8))
                : .immediate(200, body)
        }
        let model = makeModel()

        await model.send("hello")

        let failure = model.items.first { $0.kind == .failure }
        #expect(failure?.text.contains("provider_unavailable") == true)
        #expect(model.banner != nil)
        #expect(model.isStreaming == false)
    }

    @Test("a refused turn surfaces the daemon's error instead of hanging")
    func refusedTurn() async {
        defer { ErisSurfaceStub.reset() }
        ErisSurfaceStub.route = { request in
            request.url?.path == "/v1/token/refresh"
                ? .immediate(401, unauthorizedBody("refresh_token_invalid"))
                : .immediate(403, unauthorizedBody("scope_denied"))
        }
        let model = makeModel()

        await model.send("hello")

        #expect(model.isStreaming == false)
        #expect(model.items.contains { $0.kind == .failure })
        #expect(model.banner?.contains("403") == true)
    }

    @Test("an empty draft cannot be sent")
    func emptyDraftIsNotSendable() {
        let model = makeModel()
        #expect(model.canSend("") == false)
        #expect(model.canSend("   ") == false)
        #expect(model.canSend("hello") == true)
    }
    }
}

// MARK: - Classifier

@Suite("Eris delta classifier")
struct ErisDeltaClassifierTests {
    @Test("plain text stays answer text")
    func plainText() {
        var classifier = ErisDeltaClassifier()
        let emits = classifier.consume("one\ntwo\n")
        #expect(emits == [.answer("one\n"), .answer("two\n")])
    }

    @Test("a thinking fence becomes reasoning")
    func thinkingFence() {
        var classifier = ErisDeltaClassifier()
        let emits = classifier.consume("```thinking\nwhy\n```\ndone\n")
        #expect(emits == [.reasoning("why\n"), .answer("done\n")])
    }

    @Test("a tool fence becomes a run log")
    func toolFence() {
        var classifier = ErisDeltaClassifier()
        let emits = classifier.consume("```tool\nrun\n```\n")
        #expect(emits == [.toolRun("run\n")])
    }

    @Test("an ordinary code fence stays answer text")
    func codeFenceIsAnswer() {
        var classifier = ErisDeltaClassifier()
        let emits = classifier.consume("```swift\nlet x = 1\n```\n")
        #expect(emits == [.answer("```swift\n"), .answer("let x = 1\n"), .answer("```\n")])
    }

    @Test("a marker split across chunks is still recognised")
    func splitMarker() {
        var classifier = ErisDeltaClassifier()
        #expect(classifier.consume("``").isEmpty)
        let emits = classifier.consume("`thinking\nwhy\n")
        #expect(emits == [.reasoning("why\n")])
    }

    @Test("a turn that ends mid-line still shows its tail")
    func flushTail() {
        var classifier = ErisDeltaClassifier()
        _ = classifier.consume("no newline at the end")
        #expect(classifier.flush() == [.answer("no newline at the end")])
    }
}
