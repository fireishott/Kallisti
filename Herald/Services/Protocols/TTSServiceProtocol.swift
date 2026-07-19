import Foundation

@MainActor
protocol TTSServiceProtocol {
    var isPlaying: Bool { get }
    func synthesize(text: String, voice: String, context: String?) async throws -> Data
    func speak(_ text: String, voice: String, context: String?) async throws
    func stop()
}
