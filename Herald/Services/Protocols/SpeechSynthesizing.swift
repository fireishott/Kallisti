import Foundation

struct PCMChunk: Sendable {
    let data: Data  // PCM16 samples
    let sampleRate: Int  // 24000
    let channels: Int  // 1 (mono)
    let format: AudioFormat  // .pcm16
    let sequence: Int
    let isTerminal: Bool
}

enum AudioFormat: Sendable {
    case pcm16
    case wav
}

enum SpeechVoice: String, Sendable {
    case mia = "Mia"
    case chloe = "Chloe"
    case milo = "Milo"
    case dean = "Dean"
}

@MainActor
protocol SpeechSynthesizing {
    func audio(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error>
    func cancel()
}
