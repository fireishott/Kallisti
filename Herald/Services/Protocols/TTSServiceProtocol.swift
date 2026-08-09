import Foundation

@MainActor
protocol TTSServiceProtocol {
    var isPlaying: Bool { get }
    func synthesize(text: String, voice: String, context: String?) async throws -> Data
    func speak(_ text: String, voice: String, context: String?) async throws
    func stop()

    /// Called with each new token/chunk from the LLM stream.
    /// Buffers and flushes complete sentences as utterances.
    func speakStreaming(_ chunk: String, voice: String?)

    /// Called when the stream ends — flush any remaining text.
    func finishStream()
}
