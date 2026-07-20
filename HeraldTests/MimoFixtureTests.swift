import Testing
import Foundation

@Suite("MiMo API Fixture Tests")
struct MimoFixtureTests {
    @Test("ASR streaming response parses correctly")
    func parseASRResponse() throws {
        let url = Bundle.module.url(forResource: "asr_streaming_response", withExtension: "json", subdirectory: "Fixtures/Mimo")!
        let data = try Data(contentsOf: url)
        let response = try JSONDecoder().decode(ASRStreamResponse.self, from: data)
        #expect(response.stream.count == 3)
        #expect(response.stream.last?.isFinal == true)
        #expect(response.stream.last?.text == "Hello world")
    }

    @Test("TTS streaming response parses correctly")
    func parseTTSResponse() throws {
        let url = Bundle.module.url(forResource: "tts_streaming_response", withExtension: "json", subdirectory: "Fixtures/Mimo")!
        let data = try Data(contentsOf: url)
        let response = try JSONDecoder().decode(TTSStreamResponse.self, from: data)
        #expect(response.stream.count == 3)
        #expect(response.stream.last?.type == "done")
    }

    @Test("TTS error response parses correctly")
    func parseTTSError() throws {
        let url = Bundle.module.url(forResource: "tts_error_response", withExtension: "json", subdirectory: "Fixtures/Mimo")!
        let data = try Data(contentsOf: url)
        let response = try JSONDecoder().decode(MimoErrorResponse.self, from: data)
        #expect(response.error.type == "invalid_request_error")
    }
}

// Minimal models for fixture parsing
struct ASRStreamResponse: Codable, Sendable {
    let stream: [ASRDelta]
}
struct ASRDelta: Codable, Sendable {
    let type: String
    let text: String
    let isFinal: Bool
    let language: String?
    let durationMs: Int?
    enum CodingKeys: String, CodingKey {
        case type, text, isFinal = "is_final", language, durationMs = "duration_ms"
    }
}
struct TTSStreamResponse: Codable, Sendable {
    let stream: [TTSEvent]
}
struct TTSEvent: Codable, Sendable {
    let type: String
    let audio: String?
    let format: String?
    let totalDurationMs: Int?
    enum CodingKeys: String, CodingKey {
        case type, audio, format, totalDurationMs = "total_duration_ms"
    }
}
struct MimoErrorResponse: Codable, Sendable {
    let error: MimoError
}
struct MimoError: Codable, Sendable {
    let type: String
    let message: String
    let code: String?
}
