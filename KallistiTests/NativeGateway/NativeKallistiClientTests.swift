import Testing
import Foundation
import UIKit
@testable import Kallisti

/// Covers the 0.2.0 login-loop bug: `URLSessionWebSocketTask.resume()`
/// never throws, so `connect()` was declaring `.connected` before proving
/// the socket actually opened, and a transient failure afterward reset
/// `AppRootView` all the way back to onboarding (and a redundant Nous
/// OAuth login) even though the device was still validly logged in.
@Suite("NativeKallistiClient connect() status", .serialized)
struct NativeKallistiClientTests {

    /// mintTicket() does a real HTTP POST -- stub it out so tests that have
    /// a token stored don't reach the network, mirroring the pattern in
    /// NativeAuthCoordinatorTests.
    @MainActor
    private func stubbedAuthCoordinator(secureStore: MockSecureStore) async -> NativeAuthCoordinator {
        await secureStore.store(
            key: "nativeGatewayAccessTokenExpiresAt",
            value: String(Int(Date().timeIntervalSince1970) + 3_600)
        )
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubTicketURLProtocol.self]
        return NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 9119,
            session: URLSession(configuration: config),
            secureStore: secureStore
        )
    }

    // MARK: - Verification-before-connected (the false-positive itself)

    @Test("connect() does not report .connected when the post-handshake verification fails")
    @MainActor
    func connectDoesNotReportConnectedOnDeadSocket() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = await stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        // connect() itself never throws (mirrors task.resume()), but the
        // socket is actually dead -- every send fails, exactly like a
        // ws-ticket the server silently rejected.
        transport.sendError = URLError(.networkConnectionLost)

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.connectionStatus != .connected)

        await sut.disconnect()
    }

    @Test("connect() reports .connected once the verification round-trip succeeds")
    @MainActor
    func connectReportsConnectedAfterVerification() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = await stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        // First (and only) request the fresh client sends gets id 1 --
        // answer it so the verification round-trip succeeds.
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":1,"result":{}}"#.utf8))

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.connectionStatus == .connected)

        await sut.disconnect()
    }

    // MARK: - hasStoredLogin: the signal AppRootView should route on

    @Test("hasStoredLogin becomes true once connect() succeeds")
    @MainActor
    func hasStoredLoginTrueAfterSuccessfulConnect() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = await stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":1,"result":{}}"#.utf8))

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.hasStoredLogin == true)

        await sut.disconnect()
    }

    @Test("an unproven stored token that fails verification does NOT skip onboarding")
    @MainActor
    func unprovenTokenDoesNotSkipOnboarding() async throws {
        let store = MockSecureStore()
        // Simulates a stale/orphaned token left in Keychain from a previous
        // install or a previous broken build -- present, but this device
        // has never actually PROVEN it can reach the gateway with it.
        // Regression: this used to set hasStoredLogin = true just because
        // mintTicket() got past the "is there a token" check, which sent a
        // fresh install straight past onboarding into a permanently broken
        // main app with no way back -- reported live 2026-08-08.
        await store.store(key: "nativeGatewayAccessToken", value: "stale-token")
        let auth = await stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        transport.sendError = URLError(.networkConnectionLost)

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.hasStoredLogin == false)
        #expect(sut.connectionStatus == .disconnected)

        await sut.disconnect()
    }

    @Test("a failure AFTER a previously-proven connection keeps hasStoredLogin true and stays .reconnecting")
    @MainActor
    func provenConnectionSurvivesALaterFailure() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = await stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":1,"result":{}}"#.utf8))

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        // First connect proves the token actually works.
        await sut.connect()
        #expect(sut.hasStoredLogin == true)

        // A later reconnect attempt (e.g. an idle-reaped socket) fails --
        // this must NOT be treated the same as an unproven token.
        transport.sendError = URLError(.networkConnectionLost)
        await sut.connect()

        #expect(sut.connectionStatus == .reconnecting)
        #expect(sut.hasStoredLogin == true)

        await sut.disconnect()
    }

    @Test("history row identity is stable across refreshes")
    func historyRowIdentityIsStableAcrossRefreshes() {
        let first = NativeKallistiClient.historyMessageID(
            nativeSessionID: "20260813_152949_b47179",
            rowID: 483509,
            ordinal: 4,
            role: "assistant",
            content: "Photo reference task"
        )
        let second = NativeKallistiClient.historyMessageID(
            nativeSessionID: "20260813_152949_b47179",
            rowID: 483509,
            ordinal: 4,
            role: "assistant",
            content: "Photo reference task"
        )

        #expect(first == second)
    }

    @Test("different history rows never share identity even when text repeats")
    func repeatedHistoryTextKeepsDistinctRowIdentity() {
        let first = NativeKallistiClient.historyMessageID(
            nativeSessionID: "session-a",
            rowID: 100,
            ordinal: 0,
            role: "user",
            content: "same prompt"
        )
        let second = NativeKallistiClient.historyMessageID(
            nativeSessionID: "session-a",
            rowID: 101,
            ordinal: 1,
            role: "user",
            content: "same prompt"
        )

        #expect(first != second)
    }

    @Test("native image send stages bytes before prompt.submit")
    @MainActor
    func nativeImageSendStagesBytesBeforePromptSubmit() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = await stubbedAuthCoordinator(secureStore: store)
        let transport = MockNativeGatewayTransport()
        transport.onSend = { data in
            let request = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
            let id = request["id"] as! Int
            let method = request["method"] as! String
            let result: [String: Any]
            switch method {
            case "session.create":
                result = ["session_id": "native-image-session", "stored_session_id": "native-image-session"]
            default:
                result = ["status": "streaming"]
            }
            let response: [String: Any] = ["jsonrpc": "2.0", "id": id, "result": result]
            transport.queueIncoming(try! JSONSerialization.data(withJSONObject: response))
        }

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            secureStore: store,
            transportFactory: { transport }
        )
        await sut.connect()
        _ = await sut.loadConversation()
        let attachment = try #require(PendingAttachment.image(UIImage(systemName: "circle")!))
        let stream = sut.sendStreaming(
            message: "inspect this",
            attachments: [attachment],
            clientMessageID: UUID(),
            continuationContext: nil
        )
        let consumer = Task { for await _ in stream {} }
        try await Task.sleep(for: .milliseconds(150))
        consumer.cancel()

        let requests = try transport.sentFrames.map {
            try JSONSerialization.jsonObject(with: $0) as! [String: Any]
        }
        let methods = requests.compactMap { $0["method"] as? String }
        let imageIndex = try #require(methods.firstIndex(of: "image.attach_bytes"))
        let promptIndex = try #require(methods.firstIndex(of: "prompt.submit"))
        #expect(imageIndex < promptIndex)
        let imageParams = requests[imageIndex]["params"] as? [String: Any]
        #expect(imageParams?["session_id"] as? String == "native-image-session")
        #expect((imageParams?["content_base64"] as? String)?.isEmpty == false)
        #expect((imageParams?["filename"] as? String)?.hasSuffix(".jpg") == true)
        await sut.disconnect()
    }

    @Test("native MEDIA paths become authenticated inline image markdown")
    func nativeMediaPathBecomesInlineImageMarkdown() throws {
        let path = "/home/operator/.hermes/profiles/default/cache/images/funny cat.png"
        let text = "Here it is.\n\nMEDIA:\(path)\n\nDone."
        let resolved = NativeKallistiClient.resolveNativeMedia(in: text) { remotePath in
            var components = URLComponents(string: "https://hermes-relay.fihonline.net/v1/native/media")!
            components.queryItems = [URLQueryItem(name: "path", value: remotePath)]
            return components.url
        }

        #expect(!resolved.contains("MEDIA:"))
        #expect(resolved.contains("![image](https://hermes-relay.fihonline.net/v1/native/media?"))
        #expect(resolved.contains("path="))
        #expect(resolved.contains("funny%20cat.png"))
        #expect(resolved.hasSuffix("Done."))
    }

    @Test("Build 50 normalizes current and removed-profile MEDIA paths to stable keys")
    func build50MediaPathNormalization() {
        #expect(NativeKallistiClient.normalizeMediaPath(
            "/home/operator/.hermes/images/funny cat.png"
        ) == "images/funny cat.png")
        #expect(NativeKallistiClient.normalizeMediaPath(
            "/home/operator/.hermes/profiles/ignyte/images/funny cat.png"
        ) == "images/funny cat.png")
        #expect(NativeKallistiClient.normalizeMediaPath(
            "/home/operator/.hermes/profiles/default/cache/images/funny cat.png"
        ) == "cache/images/funny cat.png")
        #expect(NativeKallistiClient.normalizeMediaPath("images/../secret.png").isEmpty)
        #expect(NativeKallistiClient.normalizeMediaPath("/tmp/secret.png").isEmpty)
    }

    @Test("Build 50 historical MEDIA path becomes a stable authenticated URL")
    func build50HistoricalMediaURL() throws {
        let legacy = "/home/operator/.hermes/profiles/ignyte/images/old photo.jpg"
        let resolved = NativeKallistiClient.resolveNativeMedia(in: "MEDIA: \(legacy)") { path in
            NativeKallistiClient.nativeMediaURL(
                for: path,
                gatewayBaseURL: "https://gateway.example"
            )
        }
        let markdownURL = try #require(resolved.firstMatch(of: /\((https:\/\/[^)]+)\)/)?.1)
        let components = try #require(URLComponents(string: String(markdownURL)))
        #expect(components.path == "/v1/native/media")
        #expect(components.queryItems?.first(where: { $0.name == "path" })?.value == "images/old photo.jpg")
    }

    @Test("Build 113 preserves cross-platform MEDIA handoff for every supported attachment class")
    func build113MediaDirectiveContinuity() {
        let cases: [(path: String, kind: String, mime: String)] = [
            ("/home/agent/.hermes/images/photo.jpeg", "image", "image/jpeg"),
            ("/home/agent/.hermes/media/clip.mov", "video", "video/quicktime"),
            ("/home/agent/.hermes/attachments/report.pdf", "pdf", "application/pdf"),
            ("/home/agent/.hermes/attachments/testflight_feedback-7.zip", "file", "application/zip"),
            ("/home/agent/.hermes/attachments/voice.m4a", "audio", "audio/mp4"),
        ]
        for item in cases {
            let resolved = NativeKallistiClient.resolveHistoryMedia(in: "Sent from desktop\nMEDIA: \(item.path)\n") { URL(string: "https://relay.example/v1/native/media?path=\($0)") }
            #expect(!resolved.text.contains("MEDIA:"))
            #expect(resolved.attachments.count == 1)
            #expect(resolved.attachments.first?.kind == item.kind)
            #expect(resolved.attachments.first?.mimeType == item.mime)
        }
    }

    @Test("Build 50 connection labels are truthful and complete")
    func build50ConnectionStageLabels() {
        #expect(ConnectionStage.allCases.map(\.displayLabel) == [
            "Preparing secure session", "Contacting gateway", "Authenticating",
            "Opening secure channel", "Verifying session",
            "Restoring conversations", "Connected",
        ])
    }

    @Test("Build 50 reset is idempotent and preserves gateway credentials")
    @MainActor
    func build50ResetPreservesCredentials() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "keep-me")
        let auth = await stubbedAuthCoordinator(secureStore: store)
        let transport = MockNativeGatewayTransport()
        transport.sendError = URLError(.networkConnectionLost)
        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            secureStore: store,
            transportFactory: { transport }
        )

        async let first: Void = sut.resetConnection()
        async let second: Void = sut.resetConnection()
        _ = await (first, second)

        #expect(await store.retrieve(key: "nativeGatewayAccessToken") == "keep-me")
        await sut.disconnect()
    }

    @Test("connect() with no stored token at all leaves hasStoredLogin false and status .disconnected")
    @MainActor
    func noStoredTokenStaysDisconnected() async throws {
        let store = MockSecureStore()
        // No token stored -- never logged in.
        let auth = await stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.hasStoredLogin == false)
        #expect(sut.connectionStatus == .disconnected)

        await sut.disconnect()
    }

    @Test("restored history marks replied-to user rows delivered but final user-only row sent")
    func restoredHistoryMapsDeliveryStatus() {
        let firstUserID = UUID()
        let assistantID = UUID()
        let finalUserID = UUID()
        let firstTimestamp = Date(timeIntervalSince1970: 100)
        let assistantTimestamp = Date(timeIntervalSince1970: 101)
        let finalTimestamp = Date(timeIntervalSince1970: 102)
        let messages = [
            Message(id: firstUserID, sender: .user, content: "First", timestamp: firstTimestamp),
            Message(id: assistantID, sender: .herald, content: "Reply", timestamp: assistantTimestamp),
            Message(id: finalUserID, sender: .user, content: "Final", timestamp: finalTimestamp),
        ]

        let restored = NativeKallistiClient.mapRestoredHistoryStatuses(messages)

        #expect(restored.map(\.status) == [.delivered, .sent, .sent])
        #expect(restored.map(\.id) == [firstUserID, assistantID, finalUserID])
        #expect(restored.map(\.timestamp) == [firstTimestamp, assistantTimestamp, finalTimestamp])
        #expect(restored[1].content == "Reply")
    }

    // MARK: - facadeBaseURL: LAN vs public HTTPS

    @Test("facadeBaseURL for LAN host adds :8010")
    func facadeBaseURLForLAN() {
        let result = NativeKallistiClient.facadeBaseURL(for: "http://192.168.1.10:9119")
        #expect(result == "http://192.168.1.10:8010")
    }

    @Test("facadeBaseURL for 10.x.x.x LAN host adds :8010")
    func facadeBaseURLForRFC1910() {
        let result = NativeKallistiClient.facadeBaseURL(for: "http://10.0.0.5:9119")
        #expect(result == "http://10.0.0.5:8010")
    }

    @Test("facadeBaseURL for .local host adds :8010")
    func facadeBaseURLForMDNS() {
        let result = NativeKallistiClient.facadeBaseURL(for: "http://agent-host.local:9119")
        #expect(result == "http://agent-host.local:8010")
    }

    @Test("facadeBaseURL for public HTTPS omits port")
    func facadeBaseURLForPublicHTTPS() {
        let result = NativeKallistiClient.facadeBaseURL(for: "https://hermes-relay.fihonline.net")
        #expect(result == "https://hermes-relay.fihonline.net")
    }

    @Test("facadeBaseURL for public HTTP omits port")
    func facadeBaseURLForPublicHTTP() {
        let result = NativeKallistiClient.facadeBaseURL(for: "http://relay.example.com")
        #expect(result == "http://relay.example.com")
    }

    @Test("facadeBaseURL returns nil for invalid URL")
    func facadeBaseURLInvalid() {
        #expect(NativeKallistiClient.facadeBaseURL(for: "not-a-url") == nil)
    }

    // MARK: - isLANHost

    @Test("isLANHost detects RFC1918 and mDNS")
    func isLANHostDetection() {
        #expect(NativeKallistiClient.isLANHost("192.168.1.1"))
        #expect(NativeKallistiClient.isLANHost("10.0.0.1"))
        #expect(NativeKallistiClient.isLANHost("172.16.0.1"))
        #expect(NativeKallistiClient.isLANHost("agent-host.local"))
        #expect(!NativeKallistiClient.isLANHost("hermes-relay.fihonline.net"))
        #expect(!NativeKallistiClient.isLANHost("relay.example.com"))
    }


}

/// Answers every `/api/auth/ws-ticket` POST with a fixed ticket so
/// `mintTicket()` succeeds without touching the network. Mirrors
/// `MockTicketURLProtocol` in NativeAuthCoordinatorTests.swift.
private final class StubTicketURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.path.hasSuffix("/api/auth/ws-ticket") == true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: 200,
            httpVersion: nil,
            headerFields: nil
        )!
        let body = Data(#"{"ticket":"test-ticket"}"#.utf8)
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

// MARK: - DSH turn errors
//
// A DSH turn that ends in error must surface as a visible failure, not
// "Waiting for host..." plus a silent resend. Fixtures are the exact wire
// shapes DSH recorded for a failed MiniMax M3 turn sent from the phone.
@MainActor
struct DSHTurnErrorTests {
    let rawError = #"503: {"message":"[anthropic-compatible-1c25165c-1b1f-4db8-afc3-2f7c70834693/minimax-m3] [404]: 404 page not found (reset after 1m 45s)"}"#

    @Test func readableErrorNamesModelAndCause() {
        #expect(DSHClient.readableTurnError(rawError, model: "MM/minimax-m3")
                == "MM/minimax-m3 failed: 404 page not found. Switch models and retry.")
        #expect(DSHClient.readableTurnError("boom", model: nil)
                == "The model failed: boom. Switch models and retry.")
    }

    @Test func categoryIsNeverNil() {
        #expect(DSHClient.errorCategory(code: "SERVER", message: rawError) == "server_error")
        #expect(DSHClient.errorCategory(code: "RATE_LIMIT", message: "") == "rate_limited")
        #expect(DSHClient.errorCategory(code: nil, message: "") == "server_error")
    }

    @Test func modelNameFromSelectionEvent() throws {
        let ev = try JSONDecoder().decode(DSHClient.JSONValue.self, from: Data(
            #"{"type":"model/selection","seq":3,"data":{"provider":"ninerouter","model":"MM/minimax-m3"}}"#.utf8))
        #expect(DSHClient.modelName(fromEvent: ev) == "MM/minimax-m3")
    }

    @Test func historyRendersErroredTurn() throws {
        let obj: [String: Any] = ["type": "event", "event": [
            "type": "turn/end", "seq": 34, "time": 1790548888464,
            "data": ["turn": 1, "reason": ["kind": "error", "error": ["message": rawError, "code": "SERVER"]]],
        ]]
        let json = String(data: try JSONSerialization.data(withJSONObject: obj), encoding: .utf8)!
        let record = try JSONDecoder().decode(DSHClient.JSONValue.self, from: Data(json.utf8))
        let m = try #require(DSHClient.message(fromWireEvent: record, model: "MM/minimax-m3"))
        #expect(m.sender == .system)
        #expect(m.status == .failed)
        #expect(m.errorCategory == "server_error")
        #expect(m.content.contains("404 page not found"))
    }

    @Test func completedTurnEndNotRendered() throws {
        let record = try JSONDecoder().decode(DSHClient.JSONValue.self, from: Data(
            #"{"type":"event","event":{"type":"turn/end","seq":9,"time":1,"data":{"turn":1,"reason":{"kind":"completed"}}}}"#.utf8))
        #expect(DSHClient.message(fromWireEvent: record) == nil)
    }
}

// MARK: - DSH session identity (resume)
//
// Resuming a chat after a relaunch or from the session list must land in the
// SAME DSH session. The session id is derived from the conversation UUID and
// `stableUUID(from:)` must invert it, or a reopen forks a fresh session.
@MainActor
struct DSHSessionIdentityTests {
    @Test func sessionIdRoundTripsThroughStableUUID() {
        let conv = UUID()
        let sid = DSHClient.sessionId(for: conv)
        #expect(sid == "session-" + conv.uuidString.lowercased())
        #expect(DSHClient.stableUUID(from: sid) == conv)
    }

    @Test func sessionIdIsDeterministic() {
        let conv = UUID()
        #expect(DSHClient.sessionId(for: conv) == DSHClient.sessionId(for: conv))
    }

    @Test func jobWireDecodesInterruptedAndCompleted() throws {
        let done = try JSONDecoder().decode(DSHClient.JobWire.self, from: Data(
            #"{"status":"completed","sessionId":"session-x","turn":2,"text":"hi","messageId":"m1","usage":{"inputTokens":5,"outputTokens":3}}"#.utf8))
        #expect(done.status == "completed")
        #expect(done.turn == 2)
        #expect(done.text == "hi")
        let cut = try JSONDecoder().decode(DSHClient.JobWire.self, from: Data(
            #"{"status":"interrupted","sessionId":"session-x","turn":1,"text":null,"error":"The host restarted mid-turn (interrupted)."}"#.utf8))
        #expect(cut.status == "interrupted")
    }
}
