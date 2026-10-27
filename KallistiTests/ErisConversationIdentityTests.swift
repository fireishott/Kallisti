import Foundation
import Testing
@testable import Kallisti

// MARK: - The identity rules these tests hold
//
// One conversation, one local id. The session list, the transcript loader and
// the turn that files a message all resolve through the same binding
// (`ErisHeraldClient.conversationIDByLocalID`), and that binding is derived from
// the daemon's own id rather than minted.
//
// The two failures these cover, as reported from the phone:
//
//  1. Start a chat, get an answer, open another chat - no messages load.
//     The list rendered `stableUUID(from:)` without registering anything, so the
//     id a row carried named no conversation the loader could find and the load
//     answered with an empty "New Chat" that looked successful.
//
//  2. Come back to a chat started on the phone and it is gone.
//     The same missing binding, one relaunch later: nothing was persisted and
//     nothing healed, so the chat opened blank forever.
//
// Both are the same seam, so both tests walk the real path through
// `ErisHeraldClient` against a stubbed transport rather than a copy of the logic.

// MARK: - Stub transport

/// Answers every request from one handler. Registered on its own session, so it
/// never shares state with another suite's stub.
final class ErisIdentityURLProtocol: URLProtocol, @unchecked Sendable {
    /// Set per test; this suite is serialized, so there is one at a time.
    nonisolated(unsafe) static var handler: (@Sendable (URLRequest) throws -> (Int, Data))?

    /// Requests the transport was asked for, in order, as "METHOD path".
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

    /// How many times `path` was asked for, whatever the method.
    static func hits(_ path: String) -> Int {
        calls().filter { $0.hasSuffix(" \(path)") }.count
    }

    /// How many conversations the daemon was asked to mint.
    ///
    /// The list read shares this path, so the method is what separates "the app
    /// listed sessions" from "the app created a second conversation for a chat
    /// it had already opened".
    static func creates() -> Int {
        calls().filter { $0 == "POST /v1/conversations" }.count
    }

    private static func record(_ request: URLRequest) {
        lock.lock()
        recorded.append("\(request.httpMethod ?? "GET") \(request.url?.path ?? "")")
        lock.unlock()
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let request = self.request
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
                headerFields: ["Content-Type": "application/json"]
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

private let identityBase = URL(string: "http://192.168.1.20:7341") ?? URL(fileURLWithPath: "/")

/// The conversation the phone started, exactly as the daemon holds it: a UUIDv7
/// id, a real title, and a question/answer pair.
private let startedConversationID = "01a17da4-9d74-7c1e-9a4a-0d5b2f3e8c11"
/// A second conversation, so "open another chat" is a real second row.
private let otherConversationID = "01a17d92-2113-7b6f-8f61-2c7d9e4a5b22"

private func listBody() -> Data {
    Data(#"""
    {"ok":true,"count":2,"truncated":false,"sort":"recent","conversations":[
      {"id":"\#(otherConversationID)","title":"AI Summit Presentation Review","updated_at":"2026-10-27T14:01:16.133Z","created_at":"2026-10-27T13:34:51.155Z","project_id":null,"message_count":2},
      {"id":"\#(startedConversationID)","title":"Casual Check-In","updated_at":"2026-10-27T13:55:24.679Z","created_at":"2026-10-27T13:55:02.644Z","project_id":null,"message_count":2}
    ]}
    """#.utf8)
}

private func detailBody(id: String, title: String, question: String, answer: String) -> Data {
    Data(#"""
    {"ok":true,"id":"\#(id)","title":"\#(title)","truncated":false,"messages":[
      {"id":"m1","role":"user","body":"\#(question)","state":"complete","created_at":"2026-10-27T13:55:02.700Z","reasoning":null,"tools":[]},
      {"id":"m2","role":"assistant","body":"\#(answer)","state":"complete","created_at":"2026-10-27T13:55:24.679Z","reasoning":null,"tools":[]}
    ]}
    """#.utf8)
}

private let refreshedPairBody = Data(
    #"{"ok":true,"access_token":"access-1","refresh_token":"refresh-1","device_id":"device-1","scopes":["chat","sessions"],"expires_at":null}"#.utf8
)

/// The id the daemon mints for `POST /v1/conversations`.
private let mintedConversationID = "01a17da4-0000-7000-8000-000000000001"

private let createdConversationBody = Data(
    #"{"ok":true,"id":"01a17da4-0000-7000-8000-000000000001","title":"New chat"}"#.utf8
)

/// An isolated defaults domain, so a test never reads or writes the app's own
/// remembered daemon.
private func identityDefaults() -> UserDefaults {
    let name = "kallisti.eris.identity.\(UUID().uuidString)"
    guard let defaults = UserDefaults(suiteName: name) else { return .standard }
    defaults.removePersistentDomain(forName: name)
    return defaults
}

private func identitySession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [ErisIdentityURLProtocol.self]
    return URLSession(configuration: configuration)
}

private func identityPair() -> ErisStoredTokens {
    ErisStoredTokens(
        baseURL: identityBase,
        tokens: ErisTokenPair(
            accessToken: "access-1",
            refreshToken: "refresh-1",
            deviceID: "device-1",
            scopes: ["chat", "sessions"],
            expiresAt: nil
        )
    )
}

/// A paired client over the stub. `store` is what makes the daemon reachable;
/// the client itself is the production one.
@MainActor
private func makeClient(defaults: UserDefaults) async -> ErisHeraldClient {
    let store = ErisDaemonStore(
        tokenStore: InMemoryErisTokenStore(stored: identityPair()),
        defaults: defaults,
        session: identitySession()
    )
    let client = ErisHeraldClient(store: store, defaults: defaults)
    await client.connect()
    return client
}

/// The routes every test in this file needs: the refresh the store verifies
/// with, the conversation list, and the two transcripts.
private func installDefaultRoutes() {
    ErisIdentityURLProtocol.handler = { request in
        switch request.url?.path {
        case "/v1/token/refresh":
            return (200, refreshedPairBody)
        case "/v1/conversations":
            if request.httpMethod == "POST" {
                return (201, createdConversationBody)
            }
            return (200, listBody())
        case "/v1/conversations/\(startedConversationID)":
            return (200, detailBody(
                id: startedConversationID,
                title: "Casual Check-In",
                question: "What's good homie.",
                answer: "What's good, Curtis. Desk's open."
            ))
        case "/v1/conversations/\(mintedConversationID)":
            return (200, detailBody(
                id: mintedConversationID,
                title: "Casual Check-In",
                question: "What's good homie.",
                answer: "What's good, Curtis. Desk's open."
            ))
        case "/v1/conversations/\(otherConversationID)":
            return (200, detailBody(
                id: otherConversationID,
                title: "AI Summit Presentation Review",
                question: "Review the deck.",
                answer: "It reads in three parts."
            ))
        default:
            return (404, Data(#"{"ok":false,"code":"not_found","message":"no"}"#.utf8))
        }
    }
}

// MARK: - Tests

@MainActor
@Suite("Eris conversation identity", .serialized)
struct ErisConversationIdentityTests {

    @Test("opening another chat loads that chat's messages, not an empty one")
    func openingAnotherChatLoadsItsTranscript() async throws {
        defer { ErisIdentityURLProtocol.reset() }
        installDefaultRoutes()
        let client = await makeClient(defaults: identityDefaults())

        // The rows the sidebar renders.
        let page = try await client.listSessions(limit: 50, offset: 0, allDevices: false)
        #expect(page.sessions.count == 2)

        // The row for the chat that was started on the phone.
        let row = try #require(page.sessions.first { $0.sessionKey == startedConversationID })

        // What `switchToSession` does with it.
        let conversation = try await client.loadConversation(id: row.id)

        #expect(conversation.messages.count == 2)
        #expect(conversation.messages.first?.content == "What's good homie.")
        #expect(conversation.messages.last?.content == "What's good, Curtis. Desk's open.")
        // The row and the transcript name the same conversation.
        #expect(conversation.sessionKey == startedConversationID)

        // And the other chat is a different transcript, not the same one.
        let otherRow = try #require(page.sessions.first { $0.sessionKey == otherConversationID })
        let other = try await client.loadConversation(id: otherRow.id)
        #expect(other.sessionKey == otherConversationID)
        #expect(other.messages.last?.content == "It reads in three parts.")
    }

    @Test("a chat started on the phone comes back after a relaunch")
    func aStartedChatSurvivesARelaunch() async throws {
        defer { ErisIdentityURLProtocol.reset() }
        installDefaultRoutes()
        let defaults = identityDefaults()
        let first = await makeClient(defaults: defaults)

        // "New Chat": the UI mints the id the composer opens with, then binds it.
        let openedID = UUID()
        let summary = try await first.createSession(title: "New Chat", conversationID: openedID)
        #expect(summary.sessionKey == mintedConversationID)
        // What the app persists as the open session.
        let persistedID = summary.id

        // A relaunch: a brand-new client over the same defaults, so the only
        // thing it can know is what the first client wrote down. Nothing here
        // lists the sessions first - reopening the last chat does not go past
        // the sidebar - so this passes only because the binding was kept.
        let second = await makeClient(defaults: defaults)
        let reopened = try await second.loadConversation(id: persistedID)

        #expect(reopened.messages.count == 2)
        #expect(reopened.sessionKey == mintedConversationID)
    }

    @Test("a listed chat reopens by the id the list gave it")
    func aListedChatReopensById() async throws {
        defer { ErisIdentityURLProtocol.reset() }
        installDefaultRoutes()
        let defaults = identityDefaults()
        let first = await makeClient(defaults: defaults)

        let page = try await first.listSessions(limit: 50, offset: 0, allDevices: false)
        let row = try #require(page.sessions.first { $0.sessionKey == startedConversationID })

        // A client that has never seen this daemon: the id is derived from the
        // conversation id, so the row still names the right transcript.
        let fresh = await makeClient(defaults: identityDefaults())
        let reopened = try await fresh.loadConversation(id: row.id)

        #expect(reopened.messages.count == 2)
        #expect(reopened.sessionKey == startedConversationID)
    }

    @Test("starting a chat binds the id the composer already opened")
    func creatingASessionBindsTheOpenersID() async throws {
        defer { ErisIdentityURLProtocol.reset() }
        installDefaultRoutes()
        let client = await makeClient(defaults: identityDefaults())

        // What `createNewSession` mints and installs before it calls out.
        let openedID = UUID()
        let summary = try await client.createSession(title: "New Chat", conversationID: openedID)

        // The row keeps the id the chat is showing, instead of a second one.
        #expect(summary.id == openedID)
        #expect(summary.sessionKey == mintedConversationID)

        // Exactly one conversation was minted for this chat.
        #expect(ErisIdentityURLProtocol.creates() == 1)
    }

    @Test("binding a chat that is already on the daemon mints nothing")
    func ensuringAExistingChatMintsNothing() async throws {
        defer { ErisIdentityURLProtocol.reset() }
        installDefaultRoutes()
        let client = await makeClient(defaults: identityDefaults())

        let page = try await client.listSessions(limit: 50, offset: 0, allDevices: false)
        let row = try #require(page.sessions.first { $0.sessionKey == startedConversationID })

        let bound = await client.ensureConversation(id: row.id)

        #expect(bound)
        #expect(ErisIdentityURLProtocol.creates() == 0)
    }

    @Test("one conversation is minted per new chat, not two")
    func aNewChatMintsOneConversation() async throws {
        defer { ErisIdentityURLProtocol.reset() }
        installDefaultRoutes()
        let client = await makeClient(defaults: identityDefaults())

        // `createNewSession` mints the local id and binds it server-side.
        let openedID = UUID()
        _ = try await client.createSession(title: "New Chat", conversationID: openedID)
        // `submitNextEligible` then ensures the id it is about to send under.
        let bound = await client.ensureConversation(id: openedID)

        #expect(bound)
        // The before-fix behaviour created a second, empty "New chat" here, so
        // the transcript the operator read was not in the row they had open.
        #expect(ErisIdentityURLProtocol.creates() == 1)
    }
}

