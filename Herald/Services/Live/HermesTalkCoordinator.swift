import AVFoundation
import os

@MainActor
final class HermesTalkCoordinator {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "HermesTalk")

    enum State: Sendable, Equatable {
        case idle
        case preparing
        case listening
        case endpointing
        case transcribing
        case thinking
        case synthesizing
        case speaking
        case interrupted
        case failed(String)
        case ending

        static func == (lhs: State, rhs: State) -> Bool {
            switch (lhs, rhs) {
            case (.idle, .idle), (.preparing, .preparing), (.listening, .listening),
                 (.endpointing, .endpointing), (.transcribing, .transcribing),
                 (.thinking, .thinking), (.synthesizing, .synthesizing),
                 (.speaking, .speaking), (.interrupted, .interrupted), (.ending, .ending):
                return true
            case (.failed(let a), .failed(let b)):
                return a == b
            default:
                return false
            }
        }
    }

    private(set) var state: State = .idle

    private let capture: TalkAudioCapture
    private let asr: any SpeechRecognizing
    private let tts: MimoTTSService
    private let turnClient: TalkTurnClient
    private let playback: PCMPlaybackQueue
    let conversationId: UUID

    /// When true, automatically resumes listening after playback drains.
    var autoTurnTaking = true

    /// Active barge-in monitoring task.
    private var bargeInTask: Task<Void, Never>?

    /// Audio route/interruption observers.
    private var routeChangeObserver: NSObjectProtocol?
    private var interruptionObserver: NSObjectProtocol?

    var onStateChange: (@MainActor (State) -> Void)?
    var onTranscript: (@MainActor (TranscriptItem) -> Void)?

    init(
        capture: TalkAudioCapture,
        asr: any SpeechRecognizing,
        tts: MimoTTSService,
        turnClient: TalkTurnClient,
        playback: PCMPlaybackQueue,
        conversationId: UUID
    ) {
        self.capture = capture
        self.asr = asr
        self.tts = tts
        self.turnClient = turnClient
        self.playback = playback
        self.conversationId = conversationId
    }

    // MARK: - Public API

    func startListening() {
        switch state {
        case .idle, .interrupted, .failed:
            break
        default:
            return
        }
        state = .preparing
        notifyState()

        guard AVAudioApplication.shared.recordPermission == .granted else {
            state = .failed("Microphone access is required. Enable it in Settings.")
            notifyState()
            return
        }

        do {
            try configureAudioSessionForRecording()
            try capture.startRecording()
            state = .listening
            notifyState()
            registerAudioObservers()
        } catch {
            logger.error("Failed to start recording: \(error.localizedDescription, privacy: .public)")
            state = .failed("Mic unavailable: \(error.localizedDescription)")
            notifyState()
        }
    }

    /// Start listening with VAD-based automatic endpointing.
    /// Returns when the user stops speaking (silence detected).
    func startListeningWithVAD() async {
        switch state {
        case .idle, .interrupted, .failed:
            break
        default:
            return
        }
        state = .preparing
        notifyState()

        do {
            guard await ensureMicrophonePermission() else {
                state = .failed("Microphone access is required. Enable it in Settings.")
                notifyState()
                return
            }
            try configureAudioSessionForRecording()
            try capture.startRecording()
            state = .listening
            notifyState()
            registerAudioObservers()

            // Wait for VAD endpoint
            for await _ in capture.startListeningWithVAD() {
                guard state == .listening else { break }
                await stopListeningAndProcess()
                break
            }
        } catch {
            logger.error("Failed to start recording: \(error.localizedDescription, privacy: .public)")
            state = .failed("Mic unavailable: \(error.localizedDescription)")
            notifyState()
        }
    }

    func stopListeningAndProcess() async {
        guard case .listening = state else { return }

        state = .endpointing
        notifyState()

        guard let utterance = capture.stopRecording() else {
            state = .idle
            notifyState()
            return
        }

        // Transcribe
        state = .transcribing
        notifyState()

        var finalText = ""
        do {
            for try await update in asr.transcribe(utterance, language: .auto) {
                if update.isFinal {
                    finalText = update.text
                }
            }
        } catch {
            logger.error("ASR failed: \(error.localizedDescription, privacy: .public)")
            state = .failed("Transcription failed")
            notifyState()
            return
        }

        guard !finalText.isEmpty else {
            state = .idle
            notifyState()
            return
        }

        // Add user transcript
        let userItem = TranscriptItem(speaker: .user, text: finalText)
        onTranscript?(userItem)

        // Submit to Hermes
        state = .thinking
        notifyState()

        // Create a partial Herald transcript item for incremental streaming display.
        // This mirrors how ChatStore creates a placeholder Message before streaming.
        let heraldItemID = UUID()
        let placeholderItem = TranscriptItem(id: heraldItemID, speaker: .herald, text: "", isPartial: true)
        onTranscript?(placeholderItem)

        // Reasoning accumulates into a separate system transcript item.
        let reasoningItemID = UUID()
        var reasoningText = ""
        var hasReasoning = false

        let clientMessageId = UUID()
        var canonicalText = ""
        var pendingText = ""
        var earlySynthesisStarted = false
        var toolBoundarySeen = false
        var earlySynthesisTask: Task<Void, Never>?
        var divergence = SpeechDivergenceMetrics()

        // Coalescing: buffer deltas and flush at ~60 fps to avoid Observable churn.
        let flushIntervalNanos: UInt64 = 16_000_000  // 16ms
        var flushTask: Task<Void, Never>?

        do {
            for try await update in turnClient.submitUtterance(
                finalText,
                conversationId: conversationId,
                clientMessageId: clientMessageId
            ) {
                guard !Task.isCancelled else { break }
                switch update {
                case .jobAccepted:
                    break
                case .reasoningDelta(let delta):
                    hasReasoning = true
                    reasoningText += delta
                    let item = TranscriptItem(
                        id: reasoningItemID, speaker: .system,
                        text: "\u{1F4AD} \(reasoningText)", isPartial: true
                    )
                    onTranscript?(item)
                case .textDelta(let delta):
                    // Stop early segmentation after tool/reasoning boundaries
                    if !toolBoundarySeen {
                        pendingText += delta
                        if let boundary = SpeechTextRenderer.findSentenceBoundary(in: pendingText) {
                            let speakable = SpeechTextRenderer.render(boundary.text)
                            if !speakable.isEmpty {
                                // Prepare playback on first sentence
                                if !earlySynthesisStarted {
                                    try? playback.prepare()
                                    earlySynthesisStarted = true
                                    state = .synthesizing
                                    notifyState()
                                }
                                divergence.sentencesSpoken += 1
                                divergence.charactersSpoken += speakable.count
                                earlySynthesisTask = Task { [weak self] in
                                    guard let self else { return }
                                    do {
                                        for try await chunk in self.tts.audio(for: speakable, voice: .mia, style: nil) {
                                            guard !Task.isCancelled else { break }
                                            if chunk.isTerminal { break }
                                            self.playback.enqueue(chunk)
                                        }
                                    } catch {
                                        self.logger.error("Early TTS failed: \(error.localizedDescription, privacy: .public)")
                                    }
                                }
                            }
                            pendingText = String(pendingText[boundary.endIndex...])
                        }
                    }

                    canonicalText += delta

                    // Coalesced flush: push incremental text to the transcript
                    // at ~60 fps so the user sees word-by-word streaming.
                    flushTask?.cancel()
                    flushTask = Task { [weak self] in
                        try? await Task.sleep(nanoseconds: flushIntervalNanos)
                        guard !Task.isCancelled else { return }
                        await self?.flushPendingTranscript(
                            heraldItemID: heraldItemID,
                            canonicalText: canonicalText
                        )
                    }
                case .toolActivity(let label):
                    // Tool boundaries make text unstable — stop early segmentation
                    toolBoundarySeen = true
                    let item = TranscriptItem(speaker: .system, text: "[\(label)]", isPartial: true)
                    onTranscript?(item)
                case .completed(let text):
                    canonicalText = text
                case .cancelled:
                    flushTask?.cancel()
                    earlySynthesisTask?.cancel()
                    state = .idle
                    notifyState()
                    return
                }
            }
        } catch {
            flushTask?.cancel()
            logger.error("Hermes turn failed: \(error.localizedDescription, privacy: .public)")
            state = .failed("Hermes unavailable")
            notifyState()
            return
        }

        // Finalize reasoning if any was accumulated.
        if hasReasoning {
            let finalReasoning = TranscriptItem(
                id: reasoningItemID, speaker: .system,
                text: "\u{1F4AD} \(reasoningText)", isPartial: false
            )
            onTranscript?(finalReasoning)
        }

        // Flush final pending deltas before marking complete.
        flushTask?.cancel()
        await flushPendingTranscript(heraldItemID: heraldItemID, canonicalText: canonicalText)

        guard !canonicalText.isEmpty else {
            earlySynthesisTask?.cancel()
            if toolBoundarySeen {
                let item = TranscriptItem(
                    speaker: .system,
                    text: "Herald used tools to process your request.",
                    isPartial: false
                )
                onTranscript?(item)
            } else if hasReasoning {
                // Reasoning was already shown; nothing more to surface.
            } else {
                let item = TranscriptItem(
                    speaker: .system,
                    text: "Herald didn't produce a response. Try again.",
                    isPartial: false
                )
                onTranscript?(item)
            }
            state = .idle
            notifyState()
            return
        }

        // Add assistant transcript (finalized — no longer partial).
        let assistantItem = TranscriptItem(id: heraldItemID, speaker: .herald, text: canonicalText, isPartial: false)
        onTranscript?(assistantItem)

        // Compute divergence metrics
        let allSentences = SpeechTextRenderer.findAllSentences(in: canonicalText)
        divergence.sentencesTotal = allSentences.count
        divergence.charactersTotal = SpeechTextRenderer.render(canonicalText).count
        divergence.hadDivergence = toolBoundarySeen || (divergence.sentencesSpoken > 0 && divergence.sentencesSpoken < divergence.sentencesTotal)
        if divergence.hadDivergence {
            logger.info("Speech divergence: spoke \(divergence.sentencesSpoken)/\(divergence.sentencesTotal) sentences early")
        }

        // If early synthesis already started, wait for it and synthesize remainder
        if earlySynthesisStarted {
            state = .synthesizing
            notifyState()

            // Synthesize any remaining text after last boundary
            let remainder = SpeechTextRenderer.render(pendingText)
            if !remainder.isEmpty {
                do {
                    for try await chunk in tts.audio(for: remainder, voice: .mia, style: nil) {
                        guard !Task.isCancelled else { break }
                        if chunk.isTerminal { break }
                        playback.enqueue(chunk)
                    }
                } catch {
                    logger.error("Remainder TTS failed: \(error.localizedDescription, privacy: .public)")
                }
            }

            state = .speaking
            notifyState()

            startBargeInMonitoring()
            await playback.waitForDrain()
            stopBargeInMonitoring()
        } else {
            // No early synthesis — fall back to full synthesis
            let speakableText = SpeechTextRenderer.render(canonicalText)
            state = .synthesizing
            notifyState()

            do {
                try playback.prepare()
                for try await chunk in tts.audio(for: speakableText, voice: .mia, style: nil) {
                    guard !Task.isCancelled else { break }
                    if chunk.isTerminal { break }
                    playback.enqueue(chunk)
                }
                state = .speaking
                notifyState()

                startBargeInMonitoring()
                await playback.waitForDrain()
                stopBargeInMonitoring()
            } catch {
                logger.error("TTS failed: \(error.localizedDescription, privacy: .public)")
            }
        }

        playback.stop()

        // Auto turn-taking: automatically resume listening after playback drains
        if autoTurnTaking && state == .speaking {
            await startListeningWithVAD()
        } else {
            state = .idle
            notifyState()
        }
    }

    func interrupt() {
        bargeInTask?.cancel()
        bargeInTask = nil
        capture.cancel()
        asr.cancel()
        tts.cancel()
        playback.flush()
        state = .interrupted
        notifyState()
    }

    func endSession() {
        state = .ending
        notifyState()
        bargeInTask?.cancel()
        bargeInTask = nil
        capture.cancel()
        asr.cancel()
        tts.cancel()
        playback.stop()
        removeAudioObservers()
        deactivateAudioSession()
        state = .idle
        notifyState()
    }

    // MARK: - Barge-in

    private func startBargeInMonitoring() {
        bargeInTask?.cancel()
        bargeInTask = Task { [weak self] in
            guard let self else { return }
            for await _ in self.capture.startBargeInMonitoring() {
                guard !Task.isCancelled else { break }
                self.logger.info("Barge-in detected — stopping playback")
                await self.handleBargeIn()
                break
            }
        }
    }

    private func stopBargeInMonitoring() {
        bargeInTask?.cancel()
        bargeInTask = nil
    }

    private func handleBargeIn() async {
        // Stop playback immediately
        playback.flush()
        asr.cancel()
        tts.cancel()

        // If we have a mic tap active (from barge-in monitoring), start a fresh recording
        // for the barge-in utterance. The user was speaking, so we capture that.
        capture.cancel()

        do {
            try configureAudioSessionForRecording()
            try capture.startRecording()
            state = .listening
            notifyState()

            // Wait for VAD endpoint on the barge-in utterance
            for await _ in capture.startListeningWithVAD() {
                guard state == .listening else { break }
                await stopListeningAndProcess()
                break
            }
        } catch {
            logger.error("Barge-in re-record failed: \(error.localizedDescription, privacy: .public)")
            state = .idle
            notifyState()
        }
    }

    // MARK: - Audio Session (sole owner during Talk)

    private func ensureMicrophonePermission() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return true
        case .denied:
            return false
        case .undetermined:
            return await AVAudioApplication.requestRecordPermission()
        @unknown default:
            return false
        }
    }

    private func configureAudioSessionForRecording() throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(
            .playAndRecord,
            mode: .voiceChat,
            options: [.defaultToSpeaker, .allowBluetoothHFP]
        )
        try session.setActive(true)

        // Force speaker when no external output
        let hasExternalOutput = session.currentRoute.outputs.contains { output in
            [.headphones, .bluetoothA2DP, .bluetoothHFP, .bluetoothLE, .airPlay, .carAudio]
                .contains(output.portType)
        }
        if !hasExternalOutput {
            try session.overrideOutputAudioPort(.speaker)
        }
    }

    private func configureAudioSessionForPlayback() throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(
            .playAndRecord,
            mode: .voiceChat,
            options: [.defaultToSpeaker, .allowBluetoothHFP]
        )
        try session.setActive(true)
    }

    private func deactivateAudioSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    // MARK: - Audio Route / Interruption Handling

    private func registerAudioObservers() {
        removeAudioObservers()

        routeChangeObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            let reason = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? AVAudioSession.RouteChangeReason
            Task { @MainActor [weak self] in
                self?.handleAudioRouteChange(reason: reason)
            }
        }

        interruptionObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? AVAudioSession.InterruptionType
            Task { @MainActor [weak self] in
                self?.handleInterruption(type: type)
            }
        }
    }

    private func removeAudioObservers() {
        if let observer = routeChangeObserver {
            NotificationCenter.default.removeObserver(observer)
            routeChangeObserver = nil
        }
        if let observer = interruptionObserver {
            NotificationCenter.default.removeObserver(observer)
            interruptionObserver = nil
        }
    }

    private func handleAudioRouteChange(reason: AVAudioSession.RouteChangeReason?) {
        guard let reason else { return }
        switch reason {
        case .oldDeviceUnavailable:
            logger.info("Audio route changed: old device unavailable")
            interrupt()
        case .newDeviceAvailable:
            logger.info("Audio route changed: new device available")
            if state == .listening || state == .speaking {
                try? configureAudioSessionForRecording()
            }
        default:
            break
        }
    }

    private func handleInterruption(type: AVAudioSession.InterruptionType?) {
        guard let type else { return }
        switch type {
        case .began:
            logger.info("Audio interruption began")
            interrupt()
        case .ended:
            logger.info("Audio interruption ended")
            break
        @unknown default:
            break
        }
    }

    private func notifyState() {
        onStateChange?(state)
    }

    /// Push the current accumulated text into the partial Herald transcript item.
    /// Called on a 16ms coalescing timer during streaming to avoid Observable churn.
    private func flushPendingTranscript(heraldItemID: UUID, canonicalText: String) {
        guard !canonicalText.isEmpty else { return }
        let item = TranscriptItem(
            id: heraldItemID, speaker: .herald,
            text: canonicalText, isPartial: true
        )
        onTranscript?(item)
    }
}
