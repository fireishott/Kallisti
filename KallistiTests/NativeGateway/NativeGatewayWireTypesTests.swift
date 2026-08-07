import Testing
import Foundation
@testable import Kallisti

@Suite("Native gateway wire types")
struct NativeGatewayWireTypesTests {
    @Test("Request encodes as JSON-RPC 2.0")
    func requestEncoding() throws {
        struct Params: Encodable { let session_id: String; let text: String }
        let req = JSONRPCRequest(id: 7, method: "prompt.submit", params: Params(session_id: "abc", text: "hi"))
        let data = try JSONEncoder().encode(req)
        let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        #expect(obj?["jsonrpc"] as? String == "2.0")
        #expect(obj?["id"] as? Int == 7)
        #expect(obj?["method"] as? String == "prompt.submit")
        let params = obj?["params"] as? [String: Any]
        #expect(params?["session_id"] as? String == "abc")
    }

    @Test("Response with result decodes")
    func responseWithResult() throws {
        let json = #"{"jsonrpc":"2.0","id":3,"result":{"session_id":"xyz"}}"#
        let data = Data(json.utf8)
        let resp = try JSONDecoder().decode(NativeGatewayResponse.self, from: data)
        #expect(resp.id == 3)
        #expect(resp.error == nil)
        #expect(resp.result != nil)
    }

    @Test("Response with error decodes and is throwable")
    func responseWithError() throws {
        let json = #"{"jsonrpc":"2.0","id":3,"error":{"code":4002,"message":"text is required"}}"#
        let data = Data(json.utf8)
        let resp = try JSONDecoder().decode(NativeGatewayResponse.self, from: data)
        #expect(resp.error?.code == 4002)
        #expect(resp.error?.message == "text is required")
    }

    @Test("Event frame extracts type and session_id")
    func eventType() throws {
        let json = #"{"jsonrpc":"2.0","method":"event","params":{"type":"message.delta","session_id":"abc","payload":{"text":"Hi"}}}"#
        let data = Data(json.utf8)
        let event = try JSONDecoder().decode(NativeGatewayEvent.self, from: data)
        #expect(event.params.type == "message.delta")
        #expect(event.params.sessionId == "abc")
    }

    @Test("message.delta payload decodes text from payload.text")
    func messageDeltaPayload() throws {
        let json = #"{"text":"Hello"}"#
        let data = Data(json.utf8)
        let payload = try JSONDecoder().decode(NativeMessageDeltaPayload.self, from: data)
        #expect(payload.text == "Hello")
    }

    @Test("message.complete payload decodes text and usage")
    func messageCompletePayload() throws {
        let json = #"{"text":"Hello world","usage":{"model":"cc/claude-sonnet-5","input":100,"output":10,"total":110},"status":"complete"}"#
        let data = Data(json.utf8)
        let payload = try JSONDecoder().decode(NativeMessageCompletePayload.self, from: data)
        #expect(payload.text == "Hello world")
        #expect(payload.status == "complete")
        #expect(payload.usage?.model == "cc/claude-sonnet-5")
        #expect(payload.usage?.input == 100)
    }

    @Test("thinking.delta payload decodes text")
    func thinkingDeltaPayload() throws {
        let json = #"{"text":"mulling..."}"#
        let data = Data(json.utf8)
        let payload = try JSONDecoder().decode(NativeThinkingDeltaPayload.self, from: data)
        #expect(payload.text == "mulling...")
    }

    @Test("session.create result decodes session_id and stored_session_id")
    func sessionCreateResult() throws {
        let json = #"{"session_id":"5bab7fa9","stored_session_id":"20260807_160805_17e36a","message_count":0,"title":"test"}"#
        let data = Data(json.utf8)
        let result = try JSONDecoder().decode(NativeSessionCreateResult.self, from: data)
        #expect(result.sessionId == "5bab7fa9")
        #expect(result.storedSessionId == "20260807_160805_17e36a")
        #expect(result.title == "test")
    }
}
