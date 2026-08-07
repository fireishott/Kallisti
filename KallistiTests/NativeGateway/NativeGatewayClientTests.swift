import Testing
import Foundation
@testable import Kallisti

@Suite("NativeGatewayClient")
struct NativeGatewayClientTests {

    @Test("Request/response correlation by id")
    func requestResponseCorrelation() async throws {
        let transport = MockNativeGatewayTransport()
        let client = NativeGatewayClient(transport: transport, requestTimeout: .seconds(5))
        try await client.connect(url: URL(string: "ws://test")!)

        // Fire a request in the background
        let responseTask = Task {
            try await client.send(method: "session.list", params: EmptyParams())
        }

        // Give the send a moment to complete
        try await Task.sleep(for: .milliseconds(50))

        // Verify the transport got a valid JSON-RPC request
        let sent = transport.sentFrames
        #expect(sent.count == 1)

        let obj = try JSONSerialization.jsonObject(with: sent[0]) as! [String: Any]
        #expect(obj["method"] as? String == "session.list")
        #expect(obj["jsonrpc"] as? String == "2.0")
        let id = obj["id"] as! Int
        #expect(id == 1)

        // Queue the correlated response
        let responseJSON = #"{"jsonrpc":"2.0","id":\#(id),"result":{"sessions":[]}}"#
        transport.queueIncoming(Data(responseJSON.utf8))

        let response = try await responseTask.value
        #expect(response.id == 1)
        #expect(response.error == nil)
    }

    @Test("Multiple requests get distinct ids")
    func multipleRequestsDistinctIds() async throws {
        let transport = MockNativeGatewayTransport()
        let client = NativeGatewayClient(transport: transport, requestTimeout: .seconds(5))
        try await client.connect(url: URL(string: "ws://test")!)

        let t1 = Task { try await client.send(method: "a", params: EmptyParams()) }
        let t2 = Task { try await client.send(method: "b", params: EmptyParams()) }
        try await Task.sleep(for: .milliseconds(50))

        let sent = transport.sentFrames
        #expect(sent.count == 2)

        let obj1 = try JSONSerialization.jsonObject(with: sent[0]) as! [String: Any]
        let obj2 = try JSONSerialization.jsonObject(with: sent[1]) as! [String: Any]
        let id1 = obj1["id"] as! Int
        let id2 = obj2["id"] as! Int
        #expect(id1 != id2)

        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":\#(id1),"result":{}}"#.utf8))
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":\#(id2),"result":{}}"#.utf8))

        _ = try await t1.value
        _ = try await t2.value
    }

    @Test("Event notifications dispatch to handlers")
    func eventDispatch() async throws {
        let transport = MockNativeGatewayTransport()
        let client = NativeGatewayClient(transport: transport, requestTimeout: .seconds(5))
        try await client.connect(url: URL(string: "ws://test")!)

        let collector = EventCollector()
        await client.onEvent { event in
            collector.append(event)
        }

        let eventJSON = #"{"jsonrpc":"2.0","method":"event","params":{"type":"message.delta","session_id":"abc123","payload":{"text":"Hello"}}}"#
        transport.queueIncoming(Data(eventJSON.utf8))

        try await Task.sleep(for: .milliseconds(100))
        let events = await collector.events
        #expect(events.count == 1)
        #expect(events[0].params.type == "message.delta")
        #expect(events[0].params.sessionId == "abc123")
    }

    @Test("Request timeout fires")
    func requestTimeout() async throws {
        let transport = MockNativeGatewayTransport()
        let client = NativeGatewayClient(transport: transport, requestTimeout: .milliseconds(100))
        try await client.connect(url: URL(string: "ws://test")!)

        await #expect(throws: NativeGatewayClientError.self) {
            try await client.send(method: "slow", params: EmptyParams())
        }
    }
}

private struct EmptyParams: Encodable {}

/// Thread-safe event collector for tests.
private final class EventCollector: @unchecked Sendable {
    private var _events: [NativeGatewayEvent] = []
    private let lock = NSLock()

    func append(_ event: NativeGatewayEvent) {
        lock.lock()
        _events.append(event)
        lock.unlock()
    }

    var events: [NativeGatewayEvent] {
        lock.lock()
        defer { lock.unlock() }
        return _events
    }
}
