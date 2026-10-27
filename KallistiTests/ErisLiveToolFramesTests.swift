import Foundation
import Testing
@testable import Kallisti

// MARK: - What these hold
//
// The daemon has always known more than the phone showed. Its turn loop streams
// the round that is starting, the call that is about to run, and the bytes a
// command prints while it prints them; the device stream used to drop all three
// on the floor, so the phone could only ever say "streaming".
//
// The two symptoms that produced:
//
//  1. "I hit stop while it was thinking and something kept running, and I could
//     not see what." A round announcement and a tool start are what make thinking
//     distinguishable from working; without them a running turn and a dead one
//     look the same, and a command's two minutes of silence looks like a hang.
//  2. "I don't see the stdout." The daemon emitted it. `ErisFrame.decode` returned
//     nil for the unknown event name and the stream skipped it.
//
// These tests are the gate: the frames must decode, and they must reach the store
// as the updates it already knows how to render, on ONE row per call.

// MARK: - Stub transport

/// Answers every request from one handler, on its own session.
final class ErisTurnFrameURLProtocol: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var handler: (@Sendable (URLRequest) throws -> (Int, Data))?
    nonisolated(unsafe) private static var recorded: [String] = []
    private static let lock = NSLock()

    static func reset() {
        handler = nil
        lock.lock()
        recorded.removeAll()
        lock.unlock()
    }

    static func calls() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    private static func record(_ request: URLRequest) {
        lock.lock()
        recorded.append("\(request.httpMethod ?? "GET") \(request.url?.path ?? "")")
        lock.unlock()
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.record(request)
        guard let handler = Self.handler else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        do {
            let (status, data) = try handler(request)
            let url = request.url ?? URL(string: "http://stub.invalid") ?? URL(fileURLWithPath: "/")
            guard let response = HTTPURLResponse(
                url: url,
                statusCode: status,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "text/event-stream"]
            ) else {
                client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
                return
            }
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            if !data.isEmpty {
                client?.urlProtocol(self, didLoad: data)
            }
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    override func stopLoading() {}
}

// MARK: - Fixtures

private let frameBase = URL(string: "http://192.168.1.20:7341") ?? URL(fileURLWithPath: "/")

private func frameSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [ErisTurnFrameURLProtocol.self]
    return URLSession(configuration: configuration)
}

private func frameDefaults() -> UserDefaults {
    let name = "kallisti.eris.frames.\(UUID().uuidString)"
    guard let defaults = UserDefaults(suiteName: name) else { return .standard }
    defaults.removePersistentDomain(forName: name)
    return defaults
}

private func frameTokens() -> ErisStoredTokens {
    ErisStoredTokens(
        baseURL: frameBase,
        tokens: ErisTokenPair(
            accessToken: "access-1",
            refreshToken: "refresh-1",
            deviceID: "device-1",
            scopes: ["chat", "sessions"],
            expiresAt: nil
        )
    )
}

/// One SSE body from `(event, data)` pairs, exactly as the daemon writes them.
private func sse(_ frames: [(String, String)]) -> Data {
    var text = ""
    for frame in frames {
        text += "event: \(frame.0)\ndata: \(frame.1)\n\n"
    }
    return Data(text.utf8)
}

/// A paired client over the stub, with the daemon's own answer for the verify
/// probe and a turn whose stream carries every frame kind this change adds.
@MainActor
private func makeFrameClient(streamBody: Data) async -> ErisHeraldClient {
    let defaults = frameDefaults()
    let store = ErisDaemonStore(
        tokenStore: InMemoryErisTokenStore(stored: frameTokens()),
        defaults: defaults,
        session: frameSession()
    )
    let client = ErisHeraldClient(store: store, defaults: defaults)
    ErisTurnFrameURLProtocol.handler = { request in
        switch request.url?.path {
        case "/v1/token/refresh":
            return (200, Data(
                #"{"ok":true,"access_token":"access-1","refresh_token":"refresh-1","device_id":"device-1","scopes":["chat","sessions"],"expires_at":null}"#.utf8
            ))
        case "/v1/conversations":
            if request.httpMethod == "POST" {
                return (201, Data(#"{"ok":true,"id":"conv-live","title":"New chat"}"#.utf8))
            }
            return (200, Data(
                #"{"ok":true,"count":0,"truncated":false,"conversations":[]}"#.utf8
            ))
        case "/v1/turns":
            return (202, Data(
                #"{"job_id":"job-live","session_id":"session-1","state":"queued"}"#.utf8
            ))
        case "/v1/turns/job-live/events":
            return (200, streamBody)
        case "/v1/turns/job-live/cancel":
            return (200, Data(#"{"ok":true,"state":"cancelled"}"#.utf8))
        default:
            return (404, Data(#"{"ok":false,"code":"not_found","message":"no"}"#.utf8))
        }
    }
    await client.connect()
    return client
}

// MARK: - Tests

@MainActor
@Suite("Eris live turn frames", .serialized)
struct ErisLiveTurnFrameTests {

    /// The bridge is the exact seam the stdout died at. Decode first, in
    /// isolation, so a failure names the frame rather than the whole stream.
    @Test("the device stream's progress frames decode")
    func progressFramesDecode() throws {
        let round = try ErisFrameBridge.decode(
            event: "round",
            data: #"{"round":2,"max_rounds":12,"tools_allowed":true}"#
        )
        #expect(round == .round(ErisFrameRound(round: 2, maxRounds: 12, toolsAllowed: true)))
        #expect(!round.isTerminal)

        let start = try ErisFrameBridge.decode(
            event: "tool_start",
            data: #"{"name":"run_command","arguments":"{\"command\":\"ls\"}"}"#
        )
        #expect(start == .toolStart(ErisFrameToolStart(
            name: "run_command",
            arguments: #"{"command":"ls"}"#
        )))
        #expect(!start.isTerminal)

        let output = try ErisFrameBridge.decode(
            event: "tool_output",
            data: #"{"name":"run_command","chunk":"a.txt\nb.txt\n"}"#
        )
        #expect(output == .toolOutput(ErisFrameToolOutput(
            name: "run_command",
            chunk: "a.txt\nb.txt\n"
        )))
        #expect(!output.isTerminal)
    }

    /// The round label is what a surface shows while it waits. A closing round
    /// says it is about to answer instead of claiming another thinking pass.
    @Test("a round says whether it is still allowed to call tools")
    func roundPhaseLabel() {
        let open = ErisFrameRound(round: 2, maxRounds: 12, toolsAllowed: true)
        #expect(open.phaseLabel.contains("round 2"))
        let closing = ErisFrameRound(round: 12, maxRounds: 12, toolsAllowed: false)
        #expect(closing.phaseLabel != open.phaseLabel)
        #expect(closing.phaseLabel.lowercased().contains("answer"))
        // A daemon that does not declare a budget still gets a label.
        #expect(!ErisFrameRound(round: 1, maxRounds: 0, toolsAllowed: true).phaseLabel.isEmpty)
    }

    /// End to end through the production client: the phone's update stream has
    /// to carry the round, the row that opens the call, the bytes the command
    /// printed, and the receipt that closes that same row.
    @Test("a turn streams its round, its running command and its stdout")
    func aTurnStreamsItsRunningCommand() async {
        defer { ErisTurnFrameURLProtocol.reset() }
        let body = sse([
            ("round", #"{"round":1,"max_rounds":12,"tools_allowed":true}"#),
            ("tool_start", #"{"name":"run_command","arguments":"{\"command\":\"pytest -q\"}"}"#),
            ("tool_output", #"{"name":"run_command","chunk":"collected 3 items\n"}"#),
            ("tool_output", #"{"name":"run_command","chunk":"3 passed in 0.4s\n"}"#),
            ("tool", #"{"name":"run_command","arguments":"{\"command\":\"pytest -q\"}","output":"collected 3 items\n3 passed in 0.4s\n","is_error":false}"#),
            ("done", #"{"text":"All three pass.","usage":{"input":{"state":"reported","tokens":10},"output":{"state":"reported","tokens":5}}}"#),
        ])
        let client = await makeFrameClient(streamBody: body)

        var phases: [String] = []
        var started: [ToolActivity] = []
        var outputs: [(String, String)] = []
        var completed: [(String, String?, Bool)] = []
        for await update in client.sendStreaming(
            message: "run the tests",
            attachments: [],
            clientMessageID: UUID(),
            continuationContext: nil
        ) {
            switch update {
            case .started(let phase): phases.append(phase)
            case .toolStarted(let activity): started.append(activity)
            case .toolOutput(let id, let chunk): outputs.append((id, chunk))
            case .toolCompleted(let id, let preview, let isError, _):
                completed.append((id, preview, isError))
            default: break
            }
        }

        // The round is announced before any output, so the surface can say what
        // it is doing instead of "streaming".
        #expect(phases.count == 1)
        #expect(phases.first?.contains("round 1") == true)

        // One row opened for the call, and it is the row the output lands on.
        #expect(started.count == 1)
        let runID = started.first?.toolCallID
        #expect(runID != nil, "the started row carries the id the output is keyed by")
        #expect(started.first?.isActive == true)
        #expect(started.first?.name == "run_command")

        // The stdout the phone never saw.
        #expect(outputs.count == 2)
        #expect(outputs.allSatisfy { $0.0 == runID })
        #expect(outputs.map(\.1).joined() == "collected 3 items\n3 passed in 0.4s\n")

        // The receipt closes that same row rather than drawing a second one.
        #expect(completed.count == 1)
        #expect(completed.first?.0 == runID)
        #expect(completed.first?.1?.contains("3 passed") == true)
        #expect(completed.first?.2 == false)
        #expect(started.count == 1, "a finished call must not be a second row")
    }

    /// Stop has to reach the daemon. It used to throw `notBackedYet`, so the app
    /// tore down its own view while the turn kept running - the invisible
    /// background task, exactly.
    @Test("stopping a turn cancels it on the daemon")
    func stoppingCancelsOnTheDaemon() async {
        defer { ErisTurnFrameURLProtocol.reset() }
        // A stream that opens with one frame and then goes quiet, which is what a
        // long thinking pass looks like: nothing to render, everything still
        // running.
        let body = sse([
            ("round", #"{"round":1,"max_rounds":12,"tools_allowed":true}"#),
        ])
        let client = await makeFrameClient(streamBody: body)

        var phases: [String] = []
        for await update in client.sendStreaming(
            message: "think about it",
            attachments: [],
            clientMessageID: UUID(),
            continuationContext: nil
        ) {
            if case .started(let phase) = update { phases.append(phase) }
        }
        #expect(phases.count == 1)

        // What the composer's Stop reaches.
        let cancelled = await client.interruptSession()
        #expect(cancelled, "the daemon was asked to stop the turn")
        #expect(
            ErisTurnFrameURLProtocol.calls().contains("POST /v1/turns/job-live/cancel"),
            "a Stop is a request to the daemon, not a local teardown: \(ErisTurnFrameURLProtocol.calls())"
        )
    }

    /// A daemon that predates the start frame still renders its finished calls,
    /// so the change is additive rather than a new requirement on the daemon.
    @Test("a finished call with no start frame is still drawn")
    func aReceiptWithoutAStartStillRenders() async {
        defer { ErisTurnFrameURLProtocol.reset() }
        let body = sse([
            ("tool", #"{"name":"read_file","arguments":"{}","output":"contents","is_error":false}"#),
            ("done", #"{"text":"read it","usage":{}}"#),
        ])
        let client = await makeFrameClient(streamBody: body)

        var started: [ToolActivity] = []
        var completed = 0
        for await update in client.sendStreaming(
            message: "read the file",
            attachments: [],
            clientMessageID: UUID(),
            continuationContext: nil
        ) {
            switch update {
            case .toolStarted(let activity): started.append(activity)
            case .toolCompleted: completed += 1
            default: break
            }
        }

        #expect(started.count == 1)
        #expect(started.first?.isActive == false)
        #expect(completed == 0)
    }
}
