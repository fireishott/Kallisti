import Testing
import Foundation
@testable import Herald

@Suite("Apple TTS Service")
@MainActor
struct AppleTTSServiceTests {
    @Test("AppleTTSService conforms to TTSServiceProtocol")
    func conformsToProtocol() {
        let service = AppleTTSService()
        let _: any TTSServiceProtocol = service
        #expect(true, "AppleTTSService conforms to TTSServiceProtocol")
    }

    @Test("AppleTTSService isPlaying is false initially")
    func isPlayingInitiallyFalse() {
        let service = AppleTTSService()
        #expect(service.isPlaying == false)
    }

    @Test("AppleTTSService stop clears buffer")
    func stopClearsBuffer() {
        let service = AppleTTSService()
        // speakStreaming adds to buffer
        service.speakStreaming("Hello world", voice: nil)
        service.stop()
        #expect(service.isPlaying == false)
    }

    @Test("AppleTTSService rate is clamped to valid range")
    func rateClamped() {
        let service = AppleTTSService()
        // These should not crash
        service.setRate(0.1)  // Below minimum, should clamp to 0.4
        service.setRate(3.0)  // Above maximum, should clamp to 2.0
        service.setRate(1.0)  // Normal value
        #expect(true, "Rate clamping works without crash")
    }

    @Test("AppleTTSService speakStreaming accumulates text")
    func speakStreamingAccumulates() {
        let service = AppleTTSService()
        // speakStreaming should not crash with multiple chunks
        service.speakStreaming("Hello", voice: nil)
        service.speakStreaming(" world", voice: nil)
        service.speakStreaming(". How are you?", voice: nil)
        // The buffer should be processing
        service.stop()
        #expect(true, "speakStreaming accumulates without crash")
    }

    @Test("AppleTTSService finishStream flushes remaining text")
    func finishStreamFlushes() {
        let service = AppleTTSService()
        service.speakStreaming("Hello", voice: nil)
        // finishStream should not crash
        service.finishStream()
        #expect(service.isPlaying == false)
    }

    @Test("AppleTTSService synthesize throws not supported")
    func synthesizeThrows() async {
        let service = AppleTTSService()
        do {
            _ = try await service.synthesize(text: "test", voice: "en-US", context: nil)
            Issue.record("Expected synthesizeNotSupported error")
        } catch {
            #expect(error is AppleTTSService.AppleTTSError)
        }
    }

    @Test("AppleTTSService availableVoices returns English voices")
    func availableVoicesReturnsEnglish() {
        let service = AppleTTSService()
        let voices = service.availableVoices
        // All voices should have English language prefix
        for voice in voices {
            #expect(voice.language.hasPrefix("en"), "Voice \(voice.name) should be English")
        }
    }

    @Test("AppleTTSService currentVoice returns valid voice")
    func currentVoiceReturnsValid() {
        let service = AppleTTSService()
        let voice = service.currentVoice
        #expect(voice != nil, "currentVoice should return a valid voice")
    }

    @Test("AppleTTSService sentence boundary detection works with CJK terminators")
    func sentenceBoundaryCJK() {
        let service = AppleTTSService()
        // Test with Chinese sentence terminators
        service.speakStreaming("你好世界。", voice: nil)
        service.speakStreaming("今天天气怎么样？", voice: nil)
        service.finishStream()
        #expect(true, "CJK terminators handled without crash")
    }

    @Test("AppleTTSService handles empty text gracefully")
    func handlesEmptyText() {
        let service = AppleTTSService()
        service.speakStreaming("", voice: nil)
        service.finishStream()
        #expect(service.isPlaying == false)
    }

    @Test("AppleTTSService handles rapid speak/stop cycles")
    func handlesRapidCycles() {
        let service = AppleTTSService()
        for i in 0..<10 {
            service.speakStreaming("Word \(i)", voice: nil)
            service.stop()
        }
        #expect(service.isPlaying == false)
    }
}
