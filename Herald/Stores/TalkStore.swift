import Foundation
import Observation
import os

/// Metadata captured when a voice session completes, used to trigger transcript injection.
struct CompletedVoiceSession: Sendable {
    let voiceSessionId: UUID
    let duration: TimeInterval
    let turnCount: Int
}

@MainActor
@Observable
final class TalkStore {
    var voiceState: VoiceState = .idle
    var connectionState: TalkConnectionState = .idle
    var transcriptItems: [TranscriptItem] = []
    var sessionDuration: TimeInterval = 0
    var isMuted = false
    var isSessionActive = false
    var blockedReason: String?
    var statusMessage: String?
    var canStartSession = true
    var latencyMetrics = TalkLatencyMetrics()
    var voiceSessionID: UUID?

    /// Set after a voice session ends; consumed by MainTabView to trigger transcript injection.
    var lastCompletedSession: CompletedVoiceSession?

    /// Called when voice session state changes (start/end/state transition).
    var onSessionStateChanged: (@MainActor () -> Void)?
    @ObservationIgnored var ttsService: (any TTSServiceProtocol)?
    @ObservationIgnored var ttsSettingsProvider: (@MainActor () -> (enabled: Bool, voice: String, autoSpeak: Bool, autoSpeakDuringStreaming: Bool, appleVoiceIdentifier: String))?

    /// Hermes-native coordinator. Set via `attachHermesCoordinator()` when available.
    @ObservationIgnored var hermesCoordinator: HermesTalkCoordinator?

    /// Cached API key holder for Keychain access. Set by AppContainer.
    @ObservationIgnored var apiKeyHolder: APIKeyHolder?

    private let liveActivity = LiveActivityService()
    private var lastSpokenItemID: UUID?

    @ObservationIgnored private var durationTask: Task<Void, Never>?

    init() {}

    private func startDurationTimer() {
        durationTask?.cancel()
        sessionDuration = 0
        let started = Date()
        durationTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self, self.isSessionActive else { return }
                self.sessionDuration = Date().timeIntervalSince(started)
            }
        }
    }

    private func stopDurationTimer() {
        durationTask?.cancel()
        durationTask = nil
    }

    /// Attach a Hermes-native coordinator for push-to-talk mode.
    func attachHermesCoordinator(_ coordinator: HermesTalkCoordinator) {
        hermesCoordinator = coordinator
        coordinator.onStateChange = { [weak self] state in
            self?.applyHermesState(state)
        }
        coordinator.onTranscript = { [weak self] item in
            guard let self else { return }
            // Replace existing item with the same ID (partial updates reuse IDs)
            // so the transcript array doesn't grow unbounded during streaming.
            if let idx = self.transcriptItems.firstIndex(where: { $0.id == item.id }) {
                self.transcriptItems[idx] = item
            } else {
                self.transcriptItems.append(item)
            }
            self.onSessionStateChanged?()
        }
    }

    /// Push-to-talk: start recording (user presses mic button).
    func startListening() {
        guard let coordinator = hermesCoordinator else {
            statusMessage = "Talk coordinator not available"
            return
        }
        // Don't start if the coordinator is in a transitional state;
        // the guard in coordinator.startListening() would silently return,
        // leaving isSessionActive = true with no actual recording.
        let blockedStates: Set<HermesTalkCoordinator.State> = [
            .preparing, .listening, .endpointing, .transcribing,
            .thinking, .synthesizing, .speaking, .ending
        ]
        if blockedStates.contains(coordinator.state) {
            statusMessage = "Already active — wait for current turn to finish"
            return
        }
        isSessionActive = true
        connectionState = .connected
        voiceSessionID = coordinator.conversationId
        coordinator.startListening()
        // If coordinator rejected the start (state guard hit during
        // the async transition), surface that to the user.
        if case .failed(let msg) = coordinator.state {
            isSessionActive = false
            connectionState = .failed
            statusMessage = msg
        }
    }

    /// Push-to-talk: stop recording and process (user releases mic button).
    func stopListeningAndProcess() async {
        guard let coordinator = hermesCoordinator else { return }
        await coordinator.stopListeningAndProcess()
    }

    /// Map HermesTalkCoordinator.State to VoiceState.
    private func applyHermesState(_ state: HermesTalkCoordinator.State) {
        switch state {
        case .idle:
            voiceState = .idle
            statusMessage = nil
            isSessionActive = false
            stopDurationTimer()
        case .preparing:
            voiceState = .thinking
            statusMessage = "Preparing..."
        case .listening:
            voiceState = .listening
            statusMessage = "Listening"
        case .endpointing:
            voiceState = .transcribing
            statusMessage = "Processing..."
        case .transcribing:
            voiceState = .transcribing
            statusMessage = "Transcribing..."
        case .thinking:
            voiceState = .thinking
            statusMessage = "Thinking..."
        case .synthesizing:
            voiceState = .synthesizing
            statusMessage = "Preparing speech..."
        case .speaking:
            voiceState = .speaking
            statusMessage = "Speaking"
        case .interrupted:
            voiceState = .interrupted
            statusMessage = "Interrupted"
        case .failed(let msg):
            voiceState = .disconnected
            connectionState = .failed
            isSessionActive = false
            statusMessage = msg
            blockedReason = msg
            stopDurationTimer()
        case .ending:
            voiceState = .idle
            statusMessage = nil
            isSessionActive = false
            stopDurationTimer()
        }

        // Update Live Activity on voice state changes
        if isSessionActive {
            let status: String
            switch voiceState {
            case .listening: status = "Listening"
            case .thinking:  status = statusMessage ?? "Thinking..."
            case .speaking:  status = "Speaking"
            default:         status = statusMessage ?? "Connected"
            }
            let toolName = statusMessage?.contains("working") == true
                ? statusMessage : nil
            liveActivity.updateVoiceState(status, toolName: toolName)
        }

        onSessionStateChanged?()
        autoSpeakLatestHermesResponse()
    }

    func refreshReadiness() async {
        guard hermesCoordinator != nil else {
            canStartSession = false
            blockedReason = "Talk coordinator not available"
            return
        }
        if let apiKeyHolder {
            await apiKeyHolder.refresh()
        }
        guard let apiKeyHolder, let key = apiKeyHolder.get(), !key.isEmpty else {
            canStartSession = false
            blockedReason = "Mimo API key required — add it in Settings → Voice"
            return
        }
        canStartSession = true
        blockedReason = nil
    }

    /// Re-sync Live Activity state when returning from background.
    func handleAppDidBecomeActive() {
        liveActivity.handleAppDidBecomeActive()
    }

    /// Start without a prior readiness check — goes straight to session create.
    func startSessionDirectly() async {
        await startSession()
    }

    func startSession() async {
        guard let coordinator = hermesCoordinator else {
            blockedReason = "Talk coordinator not available"
            statusMessage = "Not ready"
            canStartSession = false
            onSessionStateChanged?()
            return
        }
        // Check for Mimo API key before starting — the coordinator will fail
        // silently without one, leaving the user confused.
        if let apiKeyHolder {
            await apiKeyHolder.refresh()
        }
        guard let apiKeyHolder, let key = apiKeyHolder.get(), !key.isEmpty else {
            blockedReason = "Mimo API key required — add it in Settings → Voice"
            statusMessage = "API key missing"
            canStartSession = false
            onSessionStateChanged?()
            return
        }
        // Start Live Activity so it appears on Lock Screen / Dynamic Island
        // during the voice session. updateVoiceState() alone is a no-op until
        // an activity has been created via startVoiceSession().
        liveActivity.startVoiceSession()

        // Do NOT await this — startListeningWithVAD() does not return until the
        // full VAD → ASR → Hermes → TTS turn completes. Awaiting it here left
        // the activation block below unreachable for the whole turn.
        Task { await coordinator.startListeningWithVAD() }

        isSessionActive = true
        connectionState = .connected
        voiceSessionID = coordinator.conversationId
        startDurationTimer()
    }

    func endSession() async {
        guard let coordinator = hermesCoordinator else { return }
        let turnCount = transcriptItems.filter { !$0.isPartial }.count
        liveActivity.endActivity()
        coordinator.endSession()
        stopDurationTimer()
        if turnCount > 0 {
            lastCompletedSession = CompletedVoiceSession(
                voiceSessionId: voiceSessionID ?? UUID(),
                duration: sessionDuration,
                turnCount: turnCount
            )
        }
        isSessionActive = false
        connectionState = .idle
    }

    func toggleMute() async {
        isMuted.toggle()
    }

    /// Manually interrupt assistant speech (e.g., from a stop button).
    func interruptAssistant() {
        hermesCoordinator?.interrupt()
    }

    /// Send an image during an active voice session.
    /// Not supported in Hermes-native Talk (no realtime vision).
    @discardableResult
    func sendImage(_ imageData: Data, triggerResponse: Bool = true) -> Bool {
        return false
    }

    func endSessionIfNeeded() async {
        guard isSessionActive else { return }
        await endSession()
    }

    func speakText(_ text: String) async {
        guard let ttsService, let settings = ttsSettingsProvider?(), settings.enabled else { return }

        // Try the primary TTS service (MiMo TTS) first
        do {
            try await ttsService.speak(text, voice: settings.voice, context: nil as String?)
            return
        } catch {
            // Log the failure and fall back to Apple TTS
            Logger.app.warning("Primary TTS failed, falling back to Apple TTS: \(error.localizedDescription)")
        }

        // Fall back to Apple TTS
        let appleTTS = AppleTTSService()
        appleTTS.setVoice(identifier: settings.appleVoiceIdentifier)
        let renderedText = SpeechTextRenderer.render(text)
        guard !renderedText.isEmpty else { return }
        do {
            try await appleTTS.speak(renderedText, voice: settings.voice, context: nil as String?)
        } catch {
            statusMessage = "TTS failed: \(error.localizedDescription)"
        }
    }

    func stopTTS() {
        ttsService?.stop()
    }

    func clearLastCompletedSession() {
        lastCompletedSession = nil
    }

    func reset() {
        voiceState = .idle
        connectionState = .idle
        transcriptItems = []
        sessionDuration = 0
        isMuted = false
        isSessionActive = false
        blockedReason = nil
        statusMessage = nil
        canStartSession = true
        latencyMetrics = TalkLatencyMetrics()
        voiceSessionID = nil
        lastCompletedSession = nil
        stopDurationTimer()
    }

    private func autoSpeakLatestHermesResponse() {
        guard let settings = ttsSettingsProvider?(), settings.enabled, settings.autoSpeak else { return }
        guard let ttsService else { return }
        guard let latestHerald = transcriptItems.last(where: { $0.speaker == .herald && !$0.isPartial }) else { return }
        guard latestHerald.id != lastSpokenItemID else { return }
        guard !latestHerald.text.isEmpty else { return }
        guard !ttsService.isPlaying else { return }
        lastSpokenItemID = latestHerald.id
        Task {
            try? await ttsService.speak(latestHerald.text, voice: settings.voice, context: nil as String?)
        }
    }
}
