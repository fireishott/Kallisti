import CarPlay
import UIKit
import os

/// Bridges `TalkStore` voice state to the CarPlay `CPVoiceControlTemplate`.
/// The current public CarPlay SDK exposes stateful voice control visuals but
/// not per-state action buttons, so this manager presents status only.
@MainActor
final class CarPlayVoiceManager {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "CarPlay")
    private static let maxTranscriptTitleLength = 80

    private let interfaceController: CPInterfaceController
    private var voiceTemplate: CPVoiceControlTemplate?
    private var observationTask: Task<Void, Never>?
    private var currentSpeakingTitle: String?
    private var lastSyncedStateID: String?
    private var talkStore: TalkStore { AppContainer.sharedDefault().talkStore }

    // MARK: - Voice Control State Identifiers

    private enum StateID {
        static let idle = "idle"
        static let listening = "listening"
        static let thinking = "thinking"
        static let speaking = "speaking"
        static let connecting = "connecting"
    }

    init(interfaceController: CPInterfaceController) {
        self.interfaceController = interfaceController
    }

    // MARK: - Lifecycle

    func configure() {
        let initialSpeakingTitle = lastAssistantText()
        currentSpeakingTitle = initialSpeakingTitle

        if talkStore.isSessionActive {
            syncState()
        }

        observationTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                self.syncState()
                // Wait until an observed property changes, then loop
                await withObservationTracking {
                    _ = self.talkStore.voiceState
                    _ = self.talkStore.connectionState
                    _ = self.talkStore.isSessionActive
                    _ = self.talkStore.transcriptItems.count
                } onChange: {}
            }
        }
    }

    func tearDown() {
        observationTask?.cancel()
        observationTask = nil
        voiceTemplate = nil
        lastSyncedStateID = nil
    }

    /// Returns the current voice control states for use in a tab bar template.
    func currentVoiceControlStates() -> [CPVoiceControlState] {
        let speakingTitle = lastAssistantText()
        let idle = CPVoiceControlState(
            identifier: StateID.idle,
            titleVariants: ["Tap Start to talk to Herald", "Talk to Herald"],
            image: UIImage(systemName: "brain.head.profile") ?? UIImage(),
            repeats: false
        )

        let connecting = CPVoiceControlState(
            identifier: StateID.connecting,
            titleVariants: ["Connecting to Herald...", "Connecting..."],
            image: UIImage(systemName: "antenna.radiowaves.left.and.right") ?? UIImage(),
            repeats: true
        )

        let listening = CPVoiceControlState(
            identifier: StateID.listening,
            titleVariants: ["Listening...", "Go ahead"],
            image: UIImage(systemName: "waveform") ?? UIImage(),
            repeats: true
        )

        let thinking = CPVoiceControlState(
            identifier: StateID.thinking,
            titleVariants: ["Thinking...", "Working on it"],
            image: UIImage(systemName: "gear") ?? UIImage(),
            repeats: true
        )

        let speaking = CPVoiceControlState(
            identifier: StateID.speaking,
            titleVariants: [speakingTitle, "Herald is speaking"],
            image: UIImage(systemName: "speaker.wave.2.fill") ?? UIImage(),
            repeats: false
        )

        return [idle, connecting, listening, thinking, speaking]
    }

    /// Sets the voice template reference for state activation.
    func setVoiceTemplate(_ template: CPVoiceControlTemplate) {
        self.voiceTemplate = template
    }

    // MARK: - State Sync

    private func currentStateIdentifier() -> String {
        guard talkStore.isSessionActive else { return StateID.idle }

        switch talkStore.connectionState {
        case .connecting, .checking:
            return StateID.connecting
        default:
            break
        }

        switch talkStore.voiceState {
        case .listening:
            return StateID.listening
        case .transcribing, .thinking:
            return StateID.thinking
        case .synthesizing, .speaking:
            return StateID.speaking
        case .interrupted:
            return StateID.listening
        case .idle, .disconnected:
            return StateID.idle
        }
    }

    private func syncState() {
        guard voiceTemplate != nil else { return }

        let stateID = currentStateIdentifier()
        let latestTitle = lastAssistantText()

        if latestTitle != currentSpeakingTitle {
            currentSpeakingTitle = latestTitle
            lastSyncedStateID = stateID
            // Update the template's speaking state title
            voiceTemplate?.activateVoiceControlState(withIdentifier: stateID)
            return
        }

        if stateID != lastSyncedStateID {
            lastSyncedStateID = stateID
            voiceTemplate?.activateVoiceControlState(withIdentifier: stateID)
        }
    }

    private func lastAssistantText() -> String {
        let lastAssistant = talkStore.transcriptItems.reversed().first(where: { $0.speaker == .herald })
        let trimmed = lastAssistant?.text.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !trimmed.isEmpty else { return "Herald is speaking" }
        return String(trimmed.prefix(Self.maxTranscriptTitleLength))
    }
}
