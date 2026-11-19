//
//  ASRRouter.swift
//  Kallisti
//
//  The microphone's counterpart to `SpeechRouter`: the daemon transcribes with
//  whatever model its provider configures, and Mimo is the alternative rather
//  than the assumption.
//

import Foundation

@MainActor
final class ASRRouter: SpeechRecognizing {
    private let preference: @MainActor () -> SpeechEnginePreference
    private let daemon: DaemonASRService
    private let mimo: MimoASRService
    private let mimoKeyPresent: @MainActor () -> Bool
    private let daemonAvailable: @MainActor () -> Bool

    private(set) var lastEngine: String?

    init(
        preference: @escaping @MainActor () -> SpeechEnginePreference,
        daemon: DaemonASRService,
        mimo: MimoASRService,
        mimoKeyPresent: @escaping @MainActor () -> Bool,
        daemonAvailable: @escaping @MainActor () -> Bool
    ) {
        self.preference = preference
        self.daemon = daemon
        self.mimo = mimo
        self.mimoKeyPresent = mimoKeyPresent
        self.daemonAvailable = daemonAvailable
    }

    func transcribe(_ utterance: RecordedUtterance, language: SpeechLanguage) -> AsyncThrowingStream<TranscriptUpdate, Error> {
        switch resolve() {
        case .mimo:
            lastEngine = SpeechEnginePreference.mimo.title
            return mimo.transcribe(utterance, language: language)
        default:
            lastEngine = SpeechEnginePreference.daemon.title
            return daemon.transcribe(utterance, language: language)
        }
    }

    func cancel() {
        daemon.cancel()
        mimo.cancel()
    }

    private func resolve() -> SpeechEnginePreference {
        switch preference() {
        case .daemon: return .daemon
        case .mimo: return mimoKeyPresent() ? .mimo : .daemon
        case .auto:
            if daemonAvailable() { return .daemon }
            return mimoKeyPresent() ? .mimo : .daemon
        }
    }
}
