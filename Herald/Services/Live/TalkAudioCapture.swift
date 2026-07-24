import AVFoundation
import Accelerate
import os

/// Non-isolated accumulator that the audio tap callback writes to directly
/// from AVAudioEngine's realtime thread.  @MainActor isolation is handled
/// by the owning `TalkAudioCapture` when it drains the accumulator on a
/// periodic flush timer.
private final class TapAccumulator: @unchecked Sendable {
    private let lock = NSLock()
    private var buffers: [AVAudioPCMBuffer] = []
    private var power: Float = -160.0

    func append(_ buffer: AVAudioPCMBuffer, power: Float) {
        lock.lock()
        buffers.append(buffer)
        self.power = power
        lock.unlock()
    }

    func swap() -> ([AVAudioPCMBuffer], Float) {
        lock.lock()
        let batch = buffers
        let pwr = power
        buffers = []
        lock.unlock()
        return (batch, pwr)
    }
}

@MainActor
final class TalkAudioCapture {
    enum CaptureError: LocalizedError {
        case noAudioInput

        var errorDescription: String? {
            switch self {
            case .noAudioInput:
                return "No microphone input is available."
            }
        }
    }

    private let logger = Logger(subsystem: "net.fihonline.herald", category: "TalkAudioCapture")

    private var audioEngine: AVAudioEngine?
    private var bargeInEngine: AVAudioEngine?
    private var recordedBuffers: [AVAudioPCMBuffer] = []
    private var isRecording = false
    private(set) var currentPower: Float = -160.0  // dBFS

    private var flushTask: Task<Void, Never>?
    private var currentAccumulator: TapAccumulator?

    // MARK: - VAD Endpointing

    private var vadEndpointContinuation: AsyncStream<Void>.Continuation?
    private var lastSpeechTime: Date?
    private let silenceThreshold: Float = -40.0  // dBFS
    private let silenceDuration: TimeInterval = 1.5
    private var vadTimer: Task<Void, Never>?

    // MARK: - Barge-in

    private let bargeInThreshold: Float = -30.0  // dBFS
    private var bargeInContinuation: AsyncStream<Void>.Continuation?

    let maxDuration: TimeInterval = 60.0
    let maxBytes: Int = 10 * 1024 * 1024  // 10 MB

    /// Non-isolated static factory for the audio tap callback.
    ///
    /// The callback runs on AVAudioEngine's realtime thread. Creating it in a
    /// `nonisolated static` context prevents Swift 6 from inferring `@MainActor`
    /// isolation on the closure, which would otherwise crash with
    /// `dispatch_assert_queue_fail` → `swift_task_checkIsolatedSwift` the moment
    /// the tap fires.
    private nonisolated static func makeTapHandler(
        accumulator: TapAccumulator
    ) -> (AVAudioPCMBuffer, AVAudioTime) -> Void {
        return { buffer, _ in
            let channelData = buffer.floatChannelData?[0]
            let frames = buffer.frameLength
            let power: Float
            if let channelData, frames > 0 {
                var rms: Float = 0
                vDSP_measqv(channelData, 1, &rms, vDSP_Length(frames))
                power = 10 * log10(rms + 1e-10)
            } else {
                power = -160.0
            }
            accumulator.append(buffer, power: power)
        }
    }

    func startRecording() throws {
        // Use cancel() for full cleanup — it nil's the engine, removes taps,
        // and clears buffers regardless of the engine's current state.
        if isRecording {
            cancel()
        }

        let engine = AVAudioEngine()
        let inputNode = engine.inputNode
        let format = inputNode.outputFormat(forBus: 0)

        // installTap(_:bufferSize:format:) raises an Objective-C exception for
        // a zero-rate/zero-channel format, which bypasses Swift's do/catch and
        // terminates the app. This can occur when permission is denied or no
        // input route is ready, so reject it before touching the audio graph.
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw CaptureError.noAudioInput
        }

        let accumulator = TapAccumulator()
        self.currentAccumulator = accumulator

        // Use the nonisolated static factory to create the tap handler so the
        // closure is NOT inferred as @MainActor — the realtime audio thread
        // never triggers a Swift 6 actor-isolation assertion.
        inputNode.installTap(
            onBus: 0, bufferSize: 1024, format: format,
            block: Self.makeTapHandler(accumulator: accumulator)
        )

        // Single flush task — drains the non-isolated accumulator and moves
        // batched data to MainActor at ~10 Hz.
        flushTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
                guard let self else { return }
                let (batch, power) = accumulator.swap()
                guard !batch.isEmpty else { continue }
                self.currentPower = power
                self.recordedBuffers.append(contentsOf: batch)
            }
        }

        engine.prepare()
        try engine.start()
        audioEngine = engine
        isRecording = true
        recordedBuffers = []
        logger.info("Started recording")
    }

    func stopRecording() -> RecordedUtterance? {
        guard isRecording else { return nil }

        flushTask?.cancel()
        flushTask = nil

        // Flush any remaining buffered audio from the non-isolated accumulator
        if let accumulator = currentAccumulator {
            let (remaining, _) = accumulator.swap()
            recordedBuffers.append(contentsOf: remaining)
        }
        currentAccumulator = nil

        audioEngine?.inputNode.removeTap(onBus: 0)
        audioEngine?.stop()
        audioEngine = nil
        isRecording = false

        guard let wavData = buffersToWAV() else {
            logger.error("Failed to convert buffers to WAV")
            return nil
        }

        guard wavData.count <= maxBytes else {
            logger.error("Recording exceeds \(self.maxBytes) byte limit: \(wavData.count) bytes")
            return nil
        }

        let duration = recordedDuration()
        guard duration <= maxDuration else {
            logger.error("Recording exceeds \(self.maxDuration)s limit")
            return nil
        }

        let sampleRate = recordedBuffers.first?.format.sampleRate ?? 24000
        logger.info("Stopped recording: \(duration)s, \(wavData.count) bytes")

        return RecordedUtterance(
            audioData: wavData,
            duration: duration,
            sampleRate: sampleRate,
            channels: 1
        )
    }

    func cancel() {
        flushTask?.cancel()
        flushTask = nil
        _ = currentAccumulator?.swap()
        currentAccumulator = nil
        audioEngine?.inputNode.removeTap(onBus: 0)
        audioEngine?.stop()
        audioEngine = nil
        stopBargeInEngine()
        isRecording = false
        recordedBuffers = []
        stopVADMonitoring()
    }

    // MARK: - VAD Endpointing

    /// Returns an AsyncStream that yields once when sustained silence is detected after speech.
    func startListeningWithVAD() -> AsyncStream<Void> {
        stopVADMonitoring()
        lastSpeechTime = nil

        return AsyncStream { continuation in
            self.vadEndpointContinuation = continuation
            self.vadTimer = Task { [weak self] in
                while !Task.isCancelled {
                    guard let self else { break }
                    try? await Task.sleep(for: .milliseconds(100))
                    guard self.isRecording else { continue }

                    if self.currentPower > self.silenceThreshold {
                        self.lastSpeechTime = Date()
                    }

                    if let lastSpeech = self.lastSpeechTime,
                       Date().timeIntervalSince(lastSpeech) > self.silenceDuration {
                        continuation.yield()
                        self.lastSpeechTime = nil
                        break
                    }
                }
            }

            continuation.onTermination = { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.stopVADMonitoring()
                }
            }
        }
    }

    /// Returns an AsyncStream that yields when speech is detected above the barge-in threshold.
    /// Used to detect the user speaking during TTS playback.
    ///
    /// Starts a lightweight recording tap solely for power monitoring (no buffer accumulation)
    /// so that barge-in works during TTS playback when the main recording engine is stopped.
    func startBargeInMonitoring() -> AsyncStream<Void> {
        // Start a fresh audio engine for power-only monitoring.
        // The main recording engine is stopped during playback, so barge-in needs its own tap.
        let engine = AVAudioEngine()
        let inputNode = engine.inputNode
        let format = inputNode.outputFormat(forBus: 0)

        guard format.sampleRate > 0, format.channelCount > 0 else {
            return AsyncStream { continuation in
                continuation.finish()
            }
        }

        var bargeInPower: Float = -160.0
        let powerQueue = DispatchQueue(label: "herald.bargein.power")

        inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            let channelData = buffer.floatChannelData?[0]
            let frames = buffer.frameLength
            if let channelData, frames > 0 {
                var rms: Float = 0
                vDSP_measqv(channelData, 1, &rms, vDSP_Length(frames))
                let power = 10 * log10(rms + 1e-10)
                powerQueue.sync { bargeInPower = power }
            }
        }

        do {
            try engine.start()
        } catch {
            return AsyncStream { continuation in
                continuation.finish()
            }
        }

        self.bargeInEngine = engine

        return AsyncStream { continuation in
            self.bargeInContinuation = continuation
            Task { [weak self] in
                while !Task.isCancelled {
                    guard let self else { break }
                    try? await Task.sleep(for: .milliseconds(100))
                    let power = powerQueue.sync { bargeInPower }
                    if power > self.bargeInThreshold {
                        continuation.yield()
                        break
                    }
                }
            }

            continuation.onTermination = { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.stopBargeInEngine()
                    self?.bargeInContinuation = nil
                }
            }
        }
    }

    /// Stop the lightweight barge-in monitoring engine.
    private func stopBargeInEngine() {
        bargeInEngine?.inputNode.removeTap(onBus: 0)
        bargeInEngine?.stop()
        bargeInEngine = nil
    }

    func stopVADMonitoring() {
        vadTimer?.cancel()
        vadTimer = nil
        vadEndpointContinuation?.finish()
        vadEndpointContinuation = nil
        bargeInContinuation?.finish()
        bargeInContinuation = nil
    }

    private func recordedDuration() -> TimeInterval {
        let totalFrames = recordedBuffers.reduce(0) { $0 + Int($1.frameLength) }
        guard let sampleRate = recordedBuffers.first?.format.sampleRate, sampleRate > 0 else { return 0 }
        return Double(totalFrames) / sampleRate
    }

    private func buffersToWAV() -> Data? {
        guard let firstBuffer = recordedBuffers.first else { return nil }
        let inputSampleRate = firstBuffer.format.sampleRate
        let totalFrames = recordedBuffers.reduce(0) { $0 + Int($1.frameLength) }

        let outputSampleRate: Double = 24000
        guard let outputFormat = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: outputSampleRate,
            channels: 1,
            interleaved: true
        ) else { return nil }

        // Resample ratio
        let ratio = outputSampleRate / inputSampleRate
        let outputFrameCount = Int(Double(totalFrames) * ratio)

        guard let outputBuffer = AVAudioPCMBuffer(
            pcmFormat: outputFormat,
            frameCapacity: AVAudioFrameCount(outputFrameCount)
        ) else { return nil }
        outputBuffer.frameLength = AVAudioFrameCount(outputFrameCount)

        guard let dst = outputBuffer.int16ChannelData?[0] else { return nil }

        // Copy, resample, and convert float→int16
        var writePosition = 0
        for buffer in recordedBuffers {
            guard let src = buffer.floatChannelData?[0] else { continue }
            let frames = Int(buffer.frameLength)
            let targetFrames = Int(Double(frames) * ratio)
            for i in 0..<targetFrames {
                let srcIndex = Float(i) / Float(ratio)
                let srcIndexInt = Int(srcIndex)
                let fraction = srcIndex - Float(srcIndexInt)
                let sampleA = srcIndexInt < frames ? src[srcIndexInt] : 0
                let sampleB = (srcIndexInt + 1) < frames ? src[srcIndexInt + 1] : sampleA
                let interpolated = sampleA + (sampleB - sampleA) * fraction
                let clamped = max(-1.0, min(1.0, interpolated))
                dst[writePosition] = Int16(clamped * 32767.0)
                writePosition += 1
            }
        }
        outputBuffer.frameLength = AVAudioFrameCount(writePosition)

        // Write WAV to temp file and read back
        let tempURL = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString + ".wav")
        defer { try? FileManager.default.removeItem(at: tempURL) }

        guard let file = try? AVAudioFile(forWriting: tempURL, settings: outputFormat.settings) else {
            logger.error("Failed to create temp WAV file")
            return nil
        }
        do {
            try file.write(from: outputBuffer)
        } catch {
            logger.error("Failed to write WAV data: \(error.localizedDescription)")
            return nil
        }
        return try? Data(contentsOf: tempURL)
    }
}
