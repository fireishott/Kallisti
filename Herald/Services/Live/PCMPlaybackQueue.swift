import AVFoundation
import os

enum PlaybackError: Error {
    case unsupportedFormat
}

@MainActor
final class PCMPlaybackQueue {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "PCMPlayback")

    private var audioEngine: AVAudioEngine?
    private var playerNode: AVAudioPlayerNode?
    private var format: AVAudioFormat?
    private var isPlaying = false
    private var scheduledCount = 0
    private var completedCount = 0
    private var drainContinuations: [CheckedContinuation<Void, Never>] = []

    func prepare() throws {
        let engine = AVAudioEngine()
        let player = AVAudioPlayerNode()
        engine.attach(player)

        guard let fmt = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: 24000,
            channels: 1,
            interleaved: true
        ) else {
            throw PlaybackError.unsupportedFormat
        }

        engine.connect(player, to: engine.mainMixerNode, format: fmt)
        try engine.start()

        audioEngine = engine
        playerNode = player
        format = fmt
        scheduledCount = 0
        completedCount = 0
    }

    func enqueue(_ chunk: PCMChunk) {
        guard let player = playerNode, let fmt = format else { return }
        guard !chunk.data.isEmpty else { return }

        let bytesPerFrame = 2  // 16-bit = 2 bytes
        let frameCount = AVAudioFrameCount(chunk.data.count / bytesPerFrame)
        guard frameCount > 0 else { return }

        guard let buffer = AVAudioPCMBuffer(pcmFormat: fmt, frameCapacity: frameCount) else { return }
        buffer.frameLength = frameCount

        chunk.data.withUnsafeBytes { rawBuffer in
            guard let src = rawBuffer.baseAddress?.assumingMemoryBound(to: Int16.self),
                  let dst = buffer.int16ChannelData?[0] else { return }
            dst.update(from: src, count: Int(frameCount))
        }

        scheduledCount += 1
        let selfRef = self
        player.scheduleBuffer(buffer) {
            Task { @MainActor [weak selfRef] in
                guard let self = selfRef else { return }
                self.completedCount += 1
                if self.completedCount >= self.scheduledCount {
                    for cont in self.drainContinuations {
                        cont.resume()
                    }
                    self.drainContinuations.removeAll()
                }
            }
        }

        if !isPlaying {
            player.play()
            isPlaying = true
        }
    }

    func flush() {
        playerNode?.stop()
        isPlaying = false
        scheduledCount = 0
        completedCount = 0
        for cont in drainContinuations {
            cont.resume()
        }
        drainContinuations.removeAll()
    }

    func stop() {
        flush()
        audioEngine?.stop()
        audioEngine = nil
        playerNode = nil
        format = nil
    }

    func waitForDrain() async {
        guard isPlaying, completedCount < scheduledCount else { return }
        await withCheckedContinuation { continuation in
            drainContinuations.append(continuation)
        }
    }
}
