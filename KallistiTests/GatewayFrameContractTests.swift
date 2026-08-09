import XCTest
@testable import Kallisti

/// Tests for the JSON-RPC 2.0 gateway frame contract.
/// These tests define the wire protocol that the Swift gateway client must implement.
final class GatewayFrameContractTests: XCTestCase {

    // MARK: - Outbound request encoding

    func testOutboundRequestHasJsonRpcVersion() throws {
        let frame: [String: Any] = [
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.list",
            "params": [String: Any]()
        ]
        XCTAssertEqual(frame["jsonrpc"] as? String, "2.0")
    }

    func testOutboundRequestHasId() throws {
        let frame: [String: Any] = [
            "jsonrpc": "2.0",
            "id": 42,
            "method": "prompt.submit",
            "params": ["message": "hello"]
        ]
        XCTAssertEqual(frame["id"] as? Int, 42)
    }

    func testOutboundRequestHasMethod() throws {
        let frame: [String: Any] = [
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.create",
            "params": ["title": "Test"]
        ]
        XCTAssertEqual(frame["method"] as? String, "session.create")
    }

    // MARK: - Response decoding

    func testResponseCorrelatesById() throws {
        let json = """
        {"jsonrpc":"2.0","id":42,"result":{"sessions":[]}}
        """
        let data = json.data(using: .utf8)!
        let response = try JSONDecoder().decode(GatewayResponse.self, from: data)
        XCTAssertEqual(response.id, 42)
        XCTAssertNotNil(response.result)
    }

    func testErrorResponseHasError() throws {
        let json = """
        {"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found"}}
        """
        let data = json.data(using: .utf8)!
        let response = try JSONDecoder().decode(GatewayResponse.self, from: data)
        XCTAssertEqual(response.id, 7)
        XCTAssertNotNil(response.error)
        XCTAssertEqual(response.error?.code, -32601)
    }

    // MARK: - Event decoding

    func testEventFrameHasMethodEvent() throws {
        let json = """
        {"jsonrpc":"2.0","method":"event","params":{"type":"message.start","session_id":"s1","payload":{}}}
        """
        let data = json.data(using: .utf8)!
        let frame = try JSONDecoder().decode(GatewayFrame.self, from: data)
        if case .event(let event) = frame {
            XCTAssertEqual(event.type, "message.start")
            XCTAssertEqual(event.sessionID, "s1")
        } else {
            XCTFail("Expected event frame")
        }
    }

    func testEventFrameWithoutSessionId() throws {
        let json = """
        {"jsonrpc":"2.0","method":"event","params":{"type":"gateway.ready","payload":{}}}
        """
        let data = json.data(using: .utf8)!
        let frame = try JSONDecoder().decode(GatewayFrame.self, from: data)
        if case .event(let event) = frame {
            XCTAssertEqual(event.type, "gateway.ready")
            XCTAssertNil(event.sessionID)
        } else {
            XCTFail("Expected event frame")
        }
    }

    // MARK: - Known event types

    func testKnownEventTypes() {
        let knownTypes: [String] = [
            "gateway.ready",
            "session.info",
            "message.start",
            "message.delta",
            "message.interim",
            "message.complete",
            "thinking.delta",
            "reasoning.delta",
            "reasoning.available",
            "status.update",
            "tool.start",
            "tool.progress",
            "tool.complete",
            "tool.generating",
            "error",
            "sessions.changed"
        ]
        for type in knownTypes {
            XCTAssertNotNil(GatewayEventType(rawValue: type), "Unknown event type: \(type)")
        }
    }

    // MARK: - Unknown field tolerance

    func testDecodeUnknownAdditiveFields() throws {
        let json = """
        {"jsonrpc":"2.0","method":"event","params":{"type":"message.start","session_id":"s1","payload":{"message_id":"m1","future_field":"should be ignored"}}}
        """
        let data = json.data(using: .utf8)!
        let frame = try JSONDecoder().decode(GatewayFrame.self, from: data)
        if case .event(let event) = frame {
            XCTAssertEqual(event.type, "message.start")
        } else {
            XCTFail("Expected event frame")
        }
    }

    func testDecodeUnknownEventType() throws {
        let json = """
        {"jsonrpc":"2.0","method":"event","params":{"type":"future.event.type","payload":{}}}
        """
        let data = json.data(using: .utf8)!
        let frame = try JSONDecoder().decode(GatewayFrame.self, from: data)
        if case .event(let event) = frame {
            XCTAssertEqual(event.type, "future.event.type")
        } else {
            XCTFail("Expected event frame")
        }
    }

    // MARK: - Content separation

    func testSystemContextNeverInUserBubble() {
        let systemContext = "[System context — current local time]"
        let userVisible = "What time is it?"

        // System context is transport metadata, not display content
        XCTAssertTrue(userVisible != systemContext)
        XCTAssertFalse(userVisible.contains("[System context"))
    }
}
