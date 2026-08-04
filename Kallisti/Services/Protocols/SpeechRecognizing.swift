import Foundation

struct RecordedUtterance: Sendable {
    let audioData: Data  // WAV format
    let duration: TimeInterval
    let sampleRate: Double
    let channels: Int
}

enum SpeechLanguage: String, Sendable {
    case auto
    case zh
    case en
}

struct TranscriptUpdate: Sendable {
    let text: String
    let isFinal: Bool
    let confidence: Float?
}

@MainActor
protocol SpeechRecognizing {
    func transcribe(_ utterance: RecordedUtterance, language: SpeechLanguage) -> AsyncThrowingStream<TranscriptUpdate, Error>
    func cancel()
}
