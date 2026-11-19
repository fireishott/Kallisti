//
//  WatchLaunchState.swift
//  KallistiWatch
//
//  The one thing a shortcut, a complication tap or a URL can ask the app to do:
//  open the conversation. Kept apart from the screen so the intent does not have
//  to know how a view is built.
//

import Foundation
import Observation

@MainActor
@Observable
final class WatchLaunchState {
    static let shared = WatchLaunchState()

    /// Set when something outside the app asked for the conversation.
    var openConversation = false
    /// Set when the caller also wants the mic open immediately.
    var startListening = false

    /// Open the conversation, and say whether to listen at once.
    func open(listening: Bool) {
        openConversation = true
        startListening = listening
    }

    /// Handle `kallisti://talk`, the URL a complication taps.
    func handle(_ url: URL) {
        guard url.scheme?.lowercased() == "kallisti" else { return }
        guard url.host?.lowercased() == "talk" || url.path == "/talk" else { return }
        open(listening: false)
    }
}
