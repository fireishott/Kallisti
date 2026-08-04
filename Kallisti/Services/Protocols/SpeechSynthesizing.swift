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

/// Mimo v2.5 built-in voices. Raw values are the wire `audio.voice` values.
/// `mimo_default` resolves per cluster — 冰糖 on the China cluster, Mia elsewhere.
enum SpeechVoice: String, Sendable, CaseIterable {
    case mimoDefault = "mimo_default"
    case mia = "Mia"
    case chloe = "Chloe"
    case milo = "Milo"
    case dean = "Dean"
    case bingtang = "冰糖"
    case moli = "茉莉"
    case suda = "苏打"
    case baihua = "白桦"
}

@MainActor
protocol SpeechSynthesizing {
    func audio(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error>
    func cancel()
}
