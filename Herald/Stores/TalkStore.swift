import Foundation
import Observation

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
    @ObservationIgnored var ttsSettingsProvider: (@MainActor () -> (enabled: Bool, voice: String, autoSpeak: Bool))?

    /// Hermes-native coordinator. Set via `attachHermesCoordinator()` when available.
    @ObservationIgnored var hermesCoordinator: HermesTalkCoordinator?

    /// Cached API key holder for Keychain access. Set by AppContainer.
    @ObservationIgnored var apiKeyHolder: APIKeyHolder?

    private let liveActivity = LiveActivityService()
    private var lastSpokenItemID: UUID?

    init() {}

    /// Attach a Hermes-native coordinator for push-to-talk mode.
    func attachHermesCoordinator(_ coordinator: HermesTalkCoordinator) {
        hermesCoordinator = coordinator
        coordinator.onStateChange = { [weak self] state in
            self?.applyHermesState(state)
        }
        coordinator.onTranscript = { [weak self] item in
            self?.transcriptItems.append(item)
            self?.onSessionStateChanged?()
        }
    }

    /// Push-to-talk: start recording (user presses mic button).
    func startListening() {
        guard let coordinator = hermesCoordinator else { return }
        isSessionActive = true
        connectionState = .connected
        voiceSessionID = coordinator.conversationId
        coordinator.startListening()
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
        case .ending:
            voiceState = .idle
            statusMessage = nil
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
        isSessionActive = true
        connectionState = .connected
        voiceSessionID = coordinator.conversationId
        await coordinator.startListeningWithVAD()
        // If the coordinator failed internally, sync state
        if case .failed(let msg) = coordinator.state {
            voiceState = .disconnected
            connectionState = .failed
            isSessionActive = false
            statusMessage = msg
            blockedReason = msg
        }
    }

    func endSession() async {
        guard let coordinator = hermesCoordinator else { return }
        let turnCount = transcriptItems.filter { !$0.isPartial }.count
        liveActivity.endActivity()
        coordinator.endSession()
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
        do {
            try await ttsService.speak(text, voice: settings.voice, context: nil as String?)
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
