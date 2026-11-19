//
//  StartConversationIntent.swift
//  KallistiWatch
//
//  The Action Button, Siri and the Shortcuts app all run this one intent. It
//  opens the app and asks for the conversation; it never starts a turn by itself,
//  because a turn is a person's words and those come from the mic.
//

import AppIntents

struct StartConversationIntent: AppIntent {
    static let title: LocalizedStringResource = "Start a conversation"
    static let description = IntentDescription(
        "Opens Kallisti on the watch and starts listening."
    )
    static let openAppWhenRun: Bool = true
    static let isDiscoverable: Bool = true

    @MainActor
    func perform() async throws -> some IntentResult {
        WatchLaunchState.shared.open(listening: true)
        return .result()
    }
}

/// The phrases Siri answers to. `\(.applicationName)` is the app's own name, so
/// the phrase survives a rename.
struct KallistiShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: StartConversationIntent(),
            phrases: [
                "Talk to \(.applicationName)",
                "Start a conversation with \(.applicationName)",
            ],
            shortTitle: "Talk",
            systemImageName: "waveform"
        )
    }
}
