//
//  WatchSpeechToText.swift
//  KallistiWatch
//
//  Speech into text, with the recognition on the host. watchOS has no `Speech`
//  framework: the wrist records 16 kHz mono, caps the capture at 45 seconds, and
//  posts the clip to the daemon's `/v1/transcribe`. That is also the better
//  answer on a wrist - the host already has whisper built and the clip never
//  leaves the machine someone owns.
//

import AVFoundation
import Foundation

@MainActor
@Observable
final class WatchSpeechToText {
    /// How long one capture may run before it ends itself.
    static let maximumCapture: Duration = .seconds(45)

    private(set) var isListening = false
    private(set) var lastErrorWord: String?

    private let engine = AVAudioEngine()
    private var file: AVAudioFile?
    private var url: URL?
    private var autoEnd: Task<Void, Never>?
    private var onFinish: (@MainActor (Data?) -> Void)?

    /// Begin a capture. `onFinish` receives the clip, or nil when nothing usable
    /// was recorded.
    func start(onFinish: @escaping @MainActor (Data?) -> Void) {
        guard !isListening else { return }
        self.onFinish = onFinish
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("clip-\(UUID().uuidString).wav")
        self.url = url

        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement)
            try session.setActive(true)
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            let file = try AVAudioFile(forWriting: url, settings: format.settings)
            self.file = file
            input.removeTap(onBus: 0)
            input.installTap(onBus: 0, bufferSize: 2048, format: format) { buffer, _ in
                try? file.write(from: buffer)
            }
            engine.prepare()
            try engine.start()
        } catch {
            lastErrorWord = "mic_unavailable"
            cleanUp()
            return
        }

        isListening = true
        autoEnd = Task { [weak self] in
            try? await Task.sleep(for: Self.maximumCapture)
            self?.stop()
        }
    }

    /// Finish the capture and hand the clip over.
    func stop() {
        autoEnd?.cancel()
        autoEnd = nil
        guard isListening else { return }
        isListening = false
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        try? AVAudioSession.sharedInstance().setActive(false)

        let clip = url.flatMap { try? Data(contentsOf: $0) }
        cleanUp()
        onFinish?(clip)
        onFinish = nil
    }

    /// Throw the capture away, which is what a cancelled turn does.
    func cancel() {
        autoEnd?.cancel()
        autoEnd = nil
        guard isListening else { return }
        isListening = false
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        try? AVAudioSession.sharedInstance().setActive(false)
        cleanUp()
        onFinish = nil
    }

    private func cleanUp() {
        file = nil
        if let url {
            try? FileManager.default.removeItem(at: url)
        }
        url = nil
    }
}
