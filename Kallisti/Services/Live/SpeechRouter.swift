//
//  SpeechRouter.swift
//  Kallisti
//
//  One place that decides which engine speaks. Auto prefers the daemon, because
//  the daemon already knows which provider is configured and a local engine
//  costs nothing and stays on the machine. Mimo is next, and only when its key
//  is set. The choice is a setting, so a person who wants a specific voice is
//  not argued with.
//

import Foundation

enum SpeechEnginePreference: String, CaseIterable, Sendable {
    case auto
    case daemon
    case mimo

    var title: String {
        switch self {
        case .auto: "Automatic"
        case .daemon: "This daemon"
        case .mimo: "Mimo cloud"
        }
    }

    var detail: String {
        switch self {
        case .auto:
            "The daemon's own engine first, which uses whatever voice provider it is configured with, then Mimo if its key is set."
        case .daemon:
            "Always ask the paired daemon. It answers with the provider it is configured for, including a local one."
        case .mimo:
            "Always use the Mimo cloud voice. Needs the Mimo API key above."
        }
    }
}

@MainActor
final class SpeechRouter: SpeechSynthesizing {
    private let preference: @MainActor () -> SpeechEnginePreference
    private let daemon: DaemonTTSService
    private let mimo: MimoTTSService
    private let mimoKeyPresent: @MainActor () -> Bool
    private let daemonAvailable: @MainActor () -> Bool

    /// The engine the last request actually used, for the settings caption and
    /// for a person who asks why the voice changed.
    private(set) var lastEngine: String?

    init(
        preference: @escaping @MainActor () -> SpeechEnginePreference,
        daemon: DaemonTTSService,
        mimo: MimoTTSService,
        mimoKeyPresent: @escaping @MainActor () -> Bool,
        daemonAvailable: @escaping @MainActor () -> Bool
    ) {
        self.preference = preference
        self.daemon = daemon
        self.mimo = mimo
        self.mimoKeyPresent = mimoKeyPresent
        self.daemonAvailable = daemonAvailable
    }

    func audio(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error> {
        let engine = resolve()
        lastEngine = engine.title
        switch engine {
        case .daemon:
            return daemon.audio(for: text, voice: voice, style: style)
        case .mimo:
            return mimo.audio(for: text, voice: voice, style: style)
        case .auto:
            // resolve() never answers auto.
            return daemon.audio(for: text, voice: voice, style: style)
        }
    }

    func cancel() {
        daemon.cancel()
        mimo.cancel()
    }

    /// Auto: daemon, then Mimo when it can speak.
    private func resolve() -> SpeechEnginePreference {
        switch preference() {
        case .daemon:
            return .daemon
        case .mimo:
            return mimoKeyPresent() ? .mimo : .daemon
        case .auto:
            if daemonAvailable() { return .daemon }
            return mimoKeyPresent() ? .mimo : .daemon
        }
    }
}
