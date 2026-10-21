import Foundation
import Testing
@testable import Kallisti

// MARK: - Helpers

/// Decode one JSON literal with a bare decoder.
///
/// The types under test carry their own `init(from:)`, so this exercises exactly
/// what the client's `decode` wrapper would do: the shape either matches the
/// daemon's contract or the type throws.
private func decodeJSON<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

// MARK: - Conversation shapes

@Suite("Eris conversation wire shapes")
struct ErisConversationDecodingTests {

    @Test("the list decodes count, truncation and a row's project and message count")
    func decodesTheList() throws {
        let json = #"""
        {"ok":true,"count":2,"truncated":false,"conversations":[
          {"id":"c1","title":"Network plan","updated_at":"2026-10-19 08:12:04","project_id":"p1","message_count":7},
          {"id":"c2","title":"New chat","updated_at":"2026-10-18 21:00:00","project_id":null,"message_count":0}
        ]}
        """#
        let page = try decodeJSON(ErisConversationPage.self, json)
        #expect(page.count == 2)
        #expect(page.truncated == false)
        #expect(page.conversations.count == 2)
        #expect(page.conversations[0].id == "c1")
        #expect(page.conversations[0].title == "Network plan")
        #expect(page.conversations[0].projectID == "p1")
        #expect(page.conversations[0].messageCount == 7)
        #expect(page.conversations[0].messageCountText == "7 messages")
        // A null project is "not filed", not a missing row.
        #expect(page.conversations[1].projectID == nil)
        #expect(page.conversations[1].messageCount == 0)
        #expect(page.conversations[1].messageCountText == "0 messages")
    }

    @Test("a cut page says so, and an empty list is still a list")
    func decodesTruncationAndEmpty() throws {
        let cut = try decodeJSON(
            ErisConversationPage.self,
            #"{"ok":true,"count":1,"truncated":true,"conversations":[{"id":"c1","title":"t","updated_at":"","project_id":null,"message_count":1}]}"#
        )
        #expect(cut.truncated)
        #expect(cut.conversations[0].messageCountText == "1 message")

        let empty = try decodeJSON(
            ErisConversationPage.self,
            #"{"ok":true,"count":0,"truncated":false,"conversations":[]}"#
        )
        #expect(empty.conversations.isEmpty)
        #expect(empty.count == 0)
    }

    @Test("a row with no id is a contract break, not a nameless row")
    func rejectsARowWithoutAnID() {
        let json = #"{"ok":true,"count":1,"truncated":false,"conversations":[{"title":"t","message_count":0}]}"#
        #expect(throws: ErisError.self) {
            _ = try decodeJSON(ErisConversationPage.self, json)
        }
    }

    @Test("the detail decodes reasoning and a tool row with its error flag")
    func decodesTheDetailWithAToolRow() throws {
        let json = #"""
        {"ok":true,"id":"c1","title":"Network plan","truncated":false,"messages":[
          {"id":"m1","role":"user","body":"check the uplink","state":"complete","created_at":"2026-10-19 08:00:00","reasoning":null,"tools":[]},
          {"id":"m2","role":"assistant","body":"The uplink is down.","state":"complete","created_at":"2026-10-19 08:01:00",
           "reasoning":"The user means the WAN link.",
           "tools":[
             {"name":"shell","arguments":"{\"cmd\":\"ping -c1 1.1.1.1\"}","output":"1 packets transmitted, 0 received","is_error":true},
             {"name":"read_file","arguments":"{\"path\":\"/etc/hosts\"}","output":"127.0.0.1 localhost","is_error":false}
           ]}
        ]}
        """#
        let detail = try decodeJSON(ErisConversationDetail.self, json)
        #expect(detail.id == "c1")
        #expect(detail.displayTitle == "Network plan")
        #expect(detail.truncated == false)
        #expect(detail.messages.count == 2)

        let question = detail.messages[0]
        #expect(question.isUser)
        #expect(question.body == "check the uplink")
        #expect(question.displayReasoning == nil)
        #expect(question.tools.isEmpty)

        let answer = detail.messages[1]
        #expect(!answer.isUser)
        #expect(answer.body == "The uplink is down.")
        #expect(answer.displayReasoning == "The user means the WAN link.")
        #expect(answer.tools.count == 2)
        #expect(answer.tools[0].name == "shell")
        #expect(answer.tools[0].arguments == #"{"cmd":"ping -c1 1.1.1.1"}"#)
        #expect(answer.tools[0].output == "1 packets transmitted, 0 received")
        #expect(answer.tools[0].isError)
        #expect(answer.tools[1].name == "read_file")
        #expect(!answer.tools[1].isError)
    }

    @Test("a detail tolerates a null reasoning, absent tools and a null title")
    func decodesAMinimalDetail() throws {
        let json = #"""
        {"ok":true,"id":"c9","title":null,"truncated":true,"messages":[
          {"id":"m1","role":"assistant","body":"","state":"failed","created_at":"","reasoning":null}
        ]}
        """#
        let detail = try decodeJSON(ErisConversationDetail.self, json)
        #expect(detail.displayTitle == "Untitled conversation")
        #expect(detail.truncated)
        #expect(detail.messages[0].tools.isEmpty)
        #expect(detail.messages[0].displayReasoning == nil)
        #expect(detail.messages[0].isFailed)
        #expect(detail.messages[0].role == "assistant")
    }

    @Test("whitespace reasoning is not a row")
    func blankReasoningIsNil() throws {
        let json = #"{"ok":true,"id":"c1","title":"t","truncated":false,"messages":[{"id":"m1","role":"assistant","body":"a","state":"complete","created_at":"","reasoning":"  \n ","tools":[]}]}"#
        let detail = try decodeJSON(ErisConversationDetail.self, json)
        #expect(detail.messages[0].displayReasoning == nil)
    }

    @Test("the created-conversation and clear bodies decode")
    func decodesCreateAndClear() throws {
        let created = try decodeJSON(
            ErisCreatedConversation.self,
            #"{"ok":true,"id":"c-new","title":"New chat"}"#
        )
        #expect(created.id == "c-new")
        #expect(created.title == "New chat")

        let cleared = try decodeJSON(ErisClearResult.self, #"{"ok":true,"cleared":12}"#)
        #expect(cleared.cleared == 12)

        let none = try decodeJSON(ErisClearResult.self, #"{"ok":true,"cleared":0}"#)
        #expect(none.cleared == 0)
    }

    @Test("the project list decodes into a name lookup")
    func decodesProjects() throws {
        let list = try decodeJSON(
            ErisProjectList.self,
            #"{"ok":true,"projects":[{"id":"p1","name":"Home lab","created_at":"2026-10-01"},{"id":"p2","name":"Work"}]}"#
        )
        #expect(list.projects.count == 2)
        #expect(list.namesByID["p1"] == "Home lab")
        #expect(list.namesByID["p2"] == "Work")
    }
}

// MARK: - Settings shapes

@Suite("Eris settings wire shapes")
struct ErisSettingsDecodingTests {

    private static let snapshot = ErisSettingsSnapshot(
        model: "ag/gemini-3-flash",
        sandbox: "read-only",
        memory: "gbrain",
        toolsEnabled: true,
        toolRounds: 6,
        approval: "ask",
        timezone: "America/Indiana/Indianapolis"
    )

    @Test("the settings body decodes every field the route reports")
    func decodesTheSnapshot() throws {
        let json = #"""
        {"ok":true,"model":"ag/gemini-3-flash","sandbox":"workspace-write","memory":"vault",
         "tools_enabled":true,"tool_rounds":8,"approval":"never","timezone":"America/Indiana/Indianapolis"}
        """#
        let settings = try decodeJSON(ErisSettingsSnapshot.self, json)
        #expect(settings.model == "ag/gemini-3-flash")
        #expect(settings.sandbox == "workspace-write")
        #expect(settings.memory == "vault")
        #expect(settings.toolsEnabled)
        #expect(settings.toolRounds == 8)
        #expect(settings.approval == "never")
        #expect(settings.timezone == "America/Indiana/Indianapolis")
    }

    @Test("an empty model is reported as chosen-none, not as a blank")
    func decodesAnEmptyModel() throws {
        let json = #"{"ok":true,"model":"","sandbox":"read-only","memory":"off","tools_enabled":false,"tool_rounds":0,"approval":"ask","timezone":""}"#
        let settings = try decodeJSON(ErisSettingsSnapshot.self, json)
        #expect(settings.displayModel == "no model chosen")
        #expect(!settings.toolsEnabled)
    }

    @Test("the update body decodes the fields the daemon actually wrote")
    func decodesTheUpdate() throws {
        let update = try decodeJSON(ErisSettingsUpdate.self, #"{"ok":true,"changed":["model","approval"]}"#)
        #expect(update.changed == ["model", "approval"])

        let unchanged = try decodeJSON(ErisSettingsUpdate.self, #"{"ok":true,"changed":[]}"#)
        #expect(unchanged.changed.isEmpty)
    }

    @Test("only the fields that differ become the patch")
    func patchHoldsOnlyChanges() {
        let patch = ErisSettingsPatch.changes(
            from: Self.snapshot,
            model: "ag/gemini-3-flash",
            sandbox: "workspace-write",
            memory: "off",
            approval: "never"
        )
        #expect(patch.sandbox == "workspace-write")
        #expect(patch.memory == "off")
        #expect(patch.approval == "never")
        // The model draft equals what is stored, so it is not in the patch.
        #expect(patch.model == nil)
        #expect(patch.changedKeys == ["sandbox", "memory", "approval"])
    }

    @Test("an untouched draft is an empty patch, which is never sent")
    func untouchedDraftIsEmpty() {
        let patch = ErisSettingsPatch.changes(
            from: Self.snapshot,
            model: Self.snapshot.model,
            sandbox: Self.snapshot.sandbox,
            memory: Self.snapshot.memory,
            approval: Self.snapshot.approval
        )
        #expect(patch.isEmpty)
        #expect(patch.changedKeys.isEmpty)
        #expect(patch.fields.isEmpty)
    }

    @Test("the model draft is trimmed before it is compared or sent")
    func modelIsTrimmed() {
        let same = ErisSettingsPatch.changes(
            from: Self.snapshot,
            model: "  ag/gemini-3-flash  ",
            sandbox: Self.snapshot.sandbox,
            memory: Self.snapshot.memory,
            approval: Self.snapshot.approval
        )
        #expect(same.isEmpty)

        let changed = ErisSettingsPatch.changes(
            from: Self.snapshot,
            model: " ds/deepseek-v4-flash ",
            sandbox: Self.snapshot.sandbox,
            memory: Self.snapshot.memory,
            approval: Self.snapshot.approval
        )
        #expect(changed.model == "ds/deepseek-v4-flash")
    }

    @Test("an empty model draft is not a change: the daemon would refuse it")
    func emptyModelIsNotSent() {
        let patch = ErisSettingsPatch.changes(
            from: Self.snapshot,
            model: "   ",
            sandbox: Self.snapshot.sandbox,
            memory: Self.snapshot.memory,
            approval: "never"
        )
        #expect(patch.model == nil)
        #expect(patch.changedKeys == ["approval"])
    }

    @Test("the POST body carries only the changed keys, in a stable order")
    func bodyCarriesOnlyChangedKeys() {
        let patch = ErisSettingsPatch.changes(
            from: Self.snapshot,
            model: Self.snapshot.model,
            sandbox: "workspace-write",
            memory: "off",
            approval: "never"
        )
        let body = ErisClient.formEncode(patch.fields)
        #expect(body == "approval=never&memory=off&sandbox=workspace-write")
        // Nothing that did not change is on the wire.
        #expect(!body.contains("model="))
    }

    @Test("the value sets are the daemon's own spellings")
    func valueSetsMatchTheDaemon() {
        #expect(ErisSettingsOption.sandbox == ["read-only", "workspace-write", "full"])
        #expect(ErisSettingsOption.approval == ["ask", "never"])
        #expect(ErisSettingsOption.memory == ["gbrain", "vault", "off"])
        #expect(ErisSettingsOption.sandboxLabel("read-only") == "Read only")
        #expect(ErisSettingsOption.approvalLabel("never") == "Never ask")
    }
}

// MARK: - Transcript rows

@Suite("Eris stored transcript rows")
struct ErisTranscriptBuilderTests {

    /// One assistant row with reasoning, a failed tool and a body.
    private static func detail() throws -> ErisConversationDetail {
        try decodeJSON(
            ErisConversationDetail.self,
            #"""
            {"ok":true,"id":"c1","title":"t","truncated":false,"messages":[
              {"id":"m1","role":"user","body":"is the uplink up?","state":"complete","created_at":"","reasoning":null,"tools":[]},
              {"id":"m2","role":"assistant","body":"It is down.","state":"complete","created_at":"",
               "reasoning":"Check the WAN first.",
               "tools":[{"name":"shell","arguments":"{\"cmd\":\"ping 1.1.1.1\"}","output":"0 received","is_error":true}]}
            ]}
            """#
        )
    }

    @Test("a stored turn becomes question, reasoning, tool run and answer rows")
    func mapsTheStoredRows() throws {
        let items = ErisTranscriptBuilder.items(for: try Self.detail())
        #expect(items.count == 4)
        #expect(items[0].kind == .question)
        #expect(items[0].text == "is the uplink up?")
        #expect(items[1].kind == .reasoning)
        #expect(items[1].text.contains("Check the WAN first."))
        #expect(items[2].kind == .toolRun)
        #expect(items[2].text.contains("shell"))
        #expect(items[2].text.contains("(failed)"))
        #expect(items[2].text.contains("0 received"))
        #expect(items[3].kind == .answer)
        #expect(items[3].text == "It is down.")
    }

    @Test("a failed turn with no reply leaves a failure row rather than a blank")
    func failedTurnIsVisible() throws {
        let detail = try decodeJSON(
            ErisConversationDetail.self,
            #"{"ok":true,"id":"c1","title":"t","truncated":false,"messages":[{"id":"m1","role":"assistant","body":"","state":"failed","created_at":"","reasoning":null,"tools":[]}]}"#
        )
        let items = ErisTranscriptBuilder.items(for: detail)
        #expect(items.count == 1)
        #expect(items[0].kind == .failure)
        #expect(!items[0].text.isEmpty)
    }

    @Test("a cut transcript says so before the first row")
    func truncatedTranscriptIsAnnounced() throws {
        let detail = try decodeJSON(
            ErisConversationDetail.self,
            #"{"ok":true,"id":"c1","title":"t","truncated":true,"messages":[{"id":"m1","role":"user","body":"hi","state":"complete","created_at":"","reasoning":null,"tools":[]}]}"#
        )
        let items = ErisTranscriptBuilder.items(for: detail)
        #expect(items.count == 2)
        #expect(items[0].kind == .notice)
        #expect(items[0].text.contains("Earlier messages"))
        #expect(items[1].kind == .question)
    }

    @Test("a tool run folds its arguments onto one line and keeps its output")
    func toolRunTextIsCompact() throws {
        let tool = ErisToolCall(
            name: "write_file",
            arguments: "{\n  \"path\": \"/tmp/x\"\n}",
            output: "wrote 12 bytes\nsecond line",
            isError: false
        )
        let text = try #require(ErisTranscriptBuilder.toolRunText([tool]))
        #expect(text.contains("\n"))
        // The arguments are one line, so the run log does not look like JSON.
        #expect(text.hasPrefix("write_file { \"path\": \"/tmp/x\" }"))
        #expect(text.contains("  wrote 12 bytes"))
        #expect(!text.contains("(failed)"))
    }

    @Test("no tools means no tool row")
    func noToolsNoRow() {
        #expect(ErisTranscriptBuilder.toolRunText([]) == nil)
    }
}

// MARK: - Failure classification

@Suite("Eris failure classification")
struct ErisFailureClassificationTests {

    @Test("a scope refusal is reported as the pairing that never had admin")
    func scopeDenial() {
        let message = "this device was paired without administration, so its token carries no admin scope"
        let failure = ErisFailureKind(
            ErisError.server(status: 403, code: "scope_denied", message: message)
        )
        #expect(failure == .adminScopeDenied(message: message))
        #expect(failure.headline == "Paired without administration")
        // The daemon's own sentence is what the screen shows.
        #expect(failure.detail == message)
        #expect(failure.needsPairing)
        // Retrying with the same token cannot work.
        #expect(!failure.canRetry)
    }

    @Test("a 401 is a dead pair, not an unreachable daemon")
    func unauthorized() {
        let failure = ErisFailureKind(
            ErisError.server(status: 401, code: "token_invalid", message: "the access token does not verify")
        )
        #expect(failure.headline == "This pairing expired")
        #expect(failure.needsPairing)
        #expect(!failure.canRetry)
        #expect(failure.code == "token_invalid")
    }

    @Test("a transport failure is the one case worth retrying")
    func transport() {
        let failure = ErisFailureKind(ErisError.transport("The request timed out."))
        #expect(failure.headline == "Can't reach the daemon")
        #expect(failure.canRetry)
        #expect(!failure.needsPairing)
    }

    @Test("a missing conversation is a typed refusal the caller can read")
    func conversationGone() {
        let failure = ErisFailureKind(
            ErisError.server(status: 404, code: "conversation_not_found", message: "no such conversation")
        )
        #expect(failure.code == "conversation_not_found")
        #expect(failure.canRetry)
        #expect(!failure.needsPairing)
    }

    @Test("a non-Eris error still reaches the screen with its own text")
    func foreignError() {
        struct Boom: Error, LocalizedError {
            var errorDescription: String? { "boom" }
        }
        let failure = ErisFailureKind(Boom())
        #expect(failure.detail == "boom")
        #expect(failure.canRetry)
    }

    @Test("the typed helpers agree with the classification")
    func typedHelpers() {
        #expect(ErisError.server(status: 403, code: "scope_denied", message: nil).isAdminScopeDenied)
        #expect(ErisError.server(status: 403, code: "scope_denied", message: nil).isForbidden)
        #expect(ErisError.server(status: 403, code: "scope_denied", message: nil).isUnauthorized == false)
        #expect(ErisError.transport("x").isTransport)
        #expect(ErisError.server(status: 500, code: nil, message: nil).isTransport == false)
    }
}

// MARK: - Routes

@Suite("Eris device routes for the rest of the API")
struct ErisDeviceRouteTests {

    @Test("every new route is under /v1 with the verb the daemon binds")
    func routesMatchTheRouter() {
        #expect(ErisRoute.conversations.path == "/v1/conversations")
        #expect(ErisRoute.conversations.method == "GET")
        #expect(ErisRoute.createConversation.path == "/v1/conversations")
        #expect(ErisRoute.createConversation.method == "POST")
        #expect(ErisRoute.conversation(conversationID: "c1").path == "/v1/conversations/c1")
        #expect(ErisRoute.conversation(conversationID: "c1").method == "GET")
        #expect(ErisRoute.clearConversation(conversationID: "c1").path == "/v1/conversations/c1/clear")
        #expect(ErisRoute.clearConversation(conversationID: "c1").method == "POST")
        #expect(ErisRoute.settings.path == "/v1/settings")
        #expect(ErisRoute.settings.method == "GET")
        #expect(ErisRoute.updateSettings.path == "/v1/settings")
        #expect(ErisRoute.updateSettings.method == "POST")
        #expect(ErisRoute.projects.path == "/v1/projects")
        #expect(ErisRoute.projects.method == "GET")
    }

    @Test("a conversation id cannot escape its path segment")
    func conversationIDsAreEscaped() {
        #expect(
            ErisRoute.conversation(conversationID: "a/b?c").path == "/v1/conversations/a%2Fb%3Fc"
        )
        #expect(
            ErisRoute.clearConversation(conversationID: "a b").path == "/v1/conversations/a%20b/clear"
        )
    }
}

// MARK: - Transport

/// Stubs every request in this file's transport suite.
///
/// A protocol of its own rather than the one the existing suites use: Swift
/// Testing runs suites in parallel, and a shared `static var handler` would let
/// two suites overwrite each other's stub.
final class ErisDeviceAPIStub: URLProtocol, @unchecked Sendable {
    /// The answer for one request, plus the body it was asked with.
    nonisolated(unsafe) static var handler: (@Sendable (URLRequest) -> (Int, Data))?
    /// Every request the stub saw, in order: method, path and body.
    nonisolated(unsafe) private static var seen: [(String, String, String)] = []
    private static let lock = NSLock()

    static func reset() {
        lock.lock()
        handler = nil
        seen.removeAll()
        lock.unlock()
    }

    static func snapshot() -> [(String, String, String)] {
        lock.lock()
        defer { lock.unlock() }
        return seen
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let request = self.request
        Self.lock.lock()
        Self.seen.append((request.httpMethod ?? "", request.url?.path ?? "", Self.body(of: request)))
        let handler = Self.handler
        Self.lock.unlock()

        guard let handler else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        let (status, data) = handler(request)
        let url = request.url ?? URL(string: "http://stub.invalid/v1/x")!
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
    }

    override func stopLoading() {}

    /// The form body, which `URLSession` hands a protocol as a stream.
    private static func body(of request: URLRequest) -> String {
        if let data = request.httpBody {
            return String(decoding: data, as: UTF8.self)
        }
        guard let stream = request.httpBodyStream else { return "" }
        stream.open()
        defer { stream.close() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 1024)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: buffer.count)
            if read <= 0 { break }
            data.append(contentsOf: buffer[0..<read])
        }
        return String(decoding: data, as: UTF8.self)
    }
}

@MainActor
@Suite("Eris device API transport", .serialized)
struct ErisDeviceAPITransportTests {
    private static let base = URL(string: "http://192.168.1.20:7341")!

    private static func stubSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ErisDeviceAPIStub.self]
        return URLSession(configuration: configuration)
    }

    private static func client() -> ErisClient {
        ErisClient(
            baseURL: base,
            tokenStore: InMemoryErisTokenStore(
                stored: ErisStoredTokens(
                    baseURL: base,
                    tokens: ErisTokenPair(
                        accessToken: "access-1",
                        refreshToken: "refresh-1",
                        scopes: ["chat", "sessions", "files"]
                    )
                )
            ),
            session: stubSession()
        )
    }

    @Test("a turn names its conversation on the wire and reports it back")
    func turnCarriesTheConversation() async throws {
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            let body = #"{"ok":true,"job_id":"job-1","session_id":"s-1","state":"queued","conversation_id":"c1"}"#
            return (202, Data(body.utf8))
        }
        defer { ErisDeviceAPIStub.reset() }

        let turn = try await Self.client().createTurn(text: "hello", conversation: "c1")
        #expect(turn.conversationID == "c1")

        let seen = ErisDeviceAPIStub.snapshot()
        #expect(seen.count == 1)
        #expect(seen[0].0 == "POST")
        #expect(seen[0].1 == "/v1/turns")
        #expect(seen[0].2 == "conversation=c1&text=hello")
    }

    @Test("a turn with no conversation does not send the field at all")
    func turnWithoutConversationOmitsTheField() async throws {
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            let body = #"{"ok":true,"job_id":"job-2","session_id":"s-2","state":"queued"}"#
            return (202, Data(body.utf8))
        }
        defer { ErisDeviceAPIStub.reset() }

        let turn = try await Self.client().createTurn(text: "hello")
        #expect(turn.conversationID == nil)

        let seen = ErisDeviceAPIStub.snapshot()
        #expect(seen[0].2 == "text=hello")
    }

    @Test("the conversation list is a GET and decodes through the client")
    func listIsAGet() async throws {
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            let body = #"{"ok":true,"count":1,"truncated":false,"conversations":[{"id":"c1","title":"t","updated_at":"","project_id":null,"message_count":3}]}"#
            return (200, Data(body.utf8))
        }
        defer { ErisDeviceAPIStub.reset() }

        let page = try await Self.client().conversations()
        #expect(page.conversations.count == 1)
        let seen = ErisDeviceAPIStub.snapshot()
        #expect(seen[0].0 == "GET")
        #expect(seen[0].1 == "/v1/conversations")
    }

    @Test("clearing a conversation posts to its clear route and reports the count")
    func clearPostsToTheClearRoute() async throws {
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            return (200, Data(#"{"ok":true,"cleared":4}"#.utf8))
        }
        defer { ErisDeviceAPIStub.reset() }

        let cleared = try await Self.client().clearConversation(id: "c1")
        #expect(cleared == 4)
        let seen = ErisDeviceAPIStub.snapshot()
        #expect(seen[0].0 == "POST")
        #expect(seen[0].1 == "/v1/conversations/c1/clear")
    }

    @Test("a settings read without admin surfaces the daemon's own sentence")
    func settingsWithoutAdmin() async throws {
        let message = "this device was paired without administration, so its token carries no admin scope and it cannot read or change the settings: mint a pairing code with the administration box ticked and pair the device again"
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            let body = "{\"ok\":false,\"code\":\"scope_denied\",\"message\":\"\(message)\"}"
            return (403, Data(body.utf8))
        }
        defer { ErisDeviceAPIStub.reset() }

        do {
            _ = try await Self.client().settings()
            Issue.record("expected a scope refusal")
        } catch let error as ErisError {
            #expect(error.status == 403)
            #expect(error.code == "scope_denied")
            #expect(error.message == message)
            #expect(ErisFailureKind(error) == .adminScopeDenied(message: message))
        } catch {
            Issue.record("expected an ErisError, got \(error)")
        }
    }

    @Test("an empty patch is refused before anything leaves the client")
    func emptyPatchIsNotSent() async throws {
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            Issue.record("an empty settings patch should not reach the daemon")
            return (400, Data())
        }
        defer { ErisDeviceAPIStub.reset() }

        do {
            _ = try await Self.client().updateSettings(ErisSettingsPatch())
            Issue.record("expected a refusal")
        } catch let error as ErisError {
            #expect(error.code == "settings_empty")
        } catch {
            Issue.record("expected an ErisError, got \(error)")
        }
        #expect(ErisDeviceAPIStub.snapshot().isEmpty)
    }

    @Test("a settings write posts only the changed fields")
    func settingsWritePostsTheSubset() async throws {
        ErisDeviceAPIStub.reset()
        ErisDeviceAPIStub.handler = { _ in
            return (200, Data(#"{"ok":true,"changed":["approval"]}"#.utf8))
        }
        defer { ErisDeviceAPIStub.reset() }

        let changed = try await Self.client().updateSettings(ErisSettingsPatch(approval: "never"))
        #expect(changed == ["approval"])
        let seen = ErisDeviceAPIStub.snapshot()
        #expect(seen[0].0 == "POST")
        #expect(seen[0].1 == "/v1/settings")
        #expect(seen[0].2 == "approval=never")
    }
}
