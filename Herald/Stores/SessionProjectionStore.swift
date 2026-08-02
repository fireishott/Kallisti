import Foundation
import os

/// Session-scoped presentation store — thin client, not agent.
///
/// This store replaces the interrupted dual-write bridge. It holds one
/// `TranscriptReducer` per runtime session. It is NOT a competing
/// conversation authority — Hermes remains the durable authority.
///
/// The store:
/// - Routes gateway events by session ID
/// - Maintains a `TranscriptReducer`-backed projection per session
/// - Publishes foreground-only changes to the UI
/// - Tracks backend busy state (authoritative from gateway events)
/// - Validates navigation generation tokens
///
/// The store does NOT:
/// - Invent prompts, agent messages, or tool decisions
/// - Create competing transcript state
/// - Mutate authoritative history
/// - Override backend busy with UI silence
actor SessionProjectionStore {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "SessionProjection")

    // MARK: - State

    /// Map of runtime session IDs to their reducer instances.
    private var reducers: [CanonicalConversationID: TranscriptReducer] = [:]

    /// The currently active (foreground) session.
    private(set) var activeSessionID: CanonicalConversationID?

    /// Current navigation epoch — incremented on every navigation change.
    private(set) var currentEpoch: NavigationEpoch = .zero

    /// Backend busy state per session (authoritative from gateway events).
    private var backendBusy: [CanonicalConversationID: Bool] = [:]

    /// The foreground projection derived from the active session's reducer.
    private(set) var foregroundProjection: TranscriptProjection?

    /// Connection state from the gateway client.
    private(set) var connectionState: GatewayClient.ConnectionState = .disconnected

    /// In-flight recovery journal — crash/reconnect aid only.
    private var recoveryJournal: [CanonicalConversationID: RecoveryJournalEntry] = [:]

    // MARK: - Session lifecycle

    /// Activate a session for foreground display.
    func activateSession(_ sessionID: CanonicalConversationID) {
        // Increment epoch
        currentEpoch = currentEpoch.incremented()
        let epoch = currentEpoch

        // Deactivate old session
        if let oldID = activeSessionID, let oldReducer = reducers[oldID] {
            Task {
                _ = try? await oldReducer.reduce(.deactivateConversation(epoch))
            }
        }

        // Activate new session
        activeSessionID = sessionID

        // Ensure reducer exists for this session
        if reducers[sessionID] == nil {
            reducers[sessionID] = TranscriptReducer()
        }

        if let reducer = reducers[sessionID] {
            Task {
                _ = try? await reducer.reduce(.activateConversation(sessionID, epoch))
                await updateForegroundProjection()
            }
        }
    }

    /// Deactivate the current session.
    func deactivateSession() {
        guard let sessionID = activeSessionID else { return }

        currentEpoch = currentEpoch.incremented()
        let epoch = currentEpoch

        if let reducer = reducers[sessionID] {
            Task {
                _ = try? await reducer.reduce(.deactivateConversation(epoch))
            }
        }

        activeSessionID = nil
        foregroundProjection = nil
    }

    // MARK: - Gateway event handling

    /// Apply a stream delta event to the appropriate session's reducer.
    func applyStreamDelta(_ delta: TranscriptReducer.TranscriptStreamDelta, epoch: NavigationEpoch) async {
        guard epoch == currentEpoch else {
            Self.logger.warning("Stale epoch \(epoch.rawValue) != current \(self.currentEpoch.rawValue)")
            return
        }

        guard let reducer = reducers[delta.conversationID] else {
            Self.logger.warning("No reducer for session \(delta.conversationID.rawValue)")
            return
        }

        _ = try? await reducer.reduce(.streamDelta(delta, epoch))

        if delta.conversationID == activeSessionID {
            await updateForegroundProjection()
        }
    }

    /// Apply a message terminal event.
    func applyMessageTerminal(_ terminal: TranscriptReducer.TranscriptTerminalEvent, epoch: NavigationEpoch) async {
        guard epoch == currentEpoch else { return }
        guard let reducer = reducers[terminal.conversationID] else { return }

        _ = try? await reducer.reduce(.messageTerminal(terminal, epoch))

        backendBusy[terminal.conversationID] = false

        if terminal.conversationID == activeSessionID {
            await updateForegroundProjection()
        }
    }

    /// Apply a user submission acceptance event.
    func applyUserAccepted(_ acceptance: TranscriptReducer.UserSubmissionAcceptance, epoch: NavigationEpoch) async {
        guard epoch == currentEpoch else { return }
        guard let reducer = reducers[acceptance.conversationID] else { return }

        _ = try? await reducer.reduce(.userSubmissionAccepted(acceptance, epoch))

        if acceptance.conversationID == activeSessionID {
            await updateForegroundProjection()
        }
    }

    /// Apply an optimistic user submission.
    func applyOptimisticUser(_ submission: TranscriptReducer.OptimisticUserSubmission, epoch: NavigationEpoch) async {
        guard epoch == currentEpoch else { return }
        guard let reducer = reducers[submission.conversationID] else { return }

        _ = try? await reducer.reduce(.optimisticUserSubmitted(submission, epoch))

        if submission.conversationID == activeSessionID {
            await updateForegroundProjection()
        }
    }

    /// Apply a snapshot (from history fetch or resume).
    func applySnapshot(_ snapshot: TranscriptReducer.TranscriptSnapshot, epoch: NavigationEpoch) async {
        guard epoch == currentEpoch else { return }
        guard let reducer = reducers[snapshot.conversationID] else { return }

        _ = try? await reducer.reduce(.snapshotReceived(snapshot, epoch))

        if snapshot.conversationID == activeSessionID {
            await updateForegroundProjection()
        }
    }

    /// Apply an assistant job binding event.
    func applyAssistantJobBound(_ binding: TranscriptReducer.AssistantJobBinding, epoch: NavigationEpoch) async {
        guard epoch == currentEpoch else { return }
        guard let reducer = reducers[binding.conversationID] else { return }

        _ = try? await reducer.reduce(.assistantJobBound(binding, epoch))

        if binding.conversationID == activeSessionID {
            await updateForegroundProjection()
        }
    }

    // MARK: - Backend state

    /// Set backend busy state for a session.
    func setBackendBusy(_ busy: Bool, for sessionID: CanonicalConversationID) {
        backendBusy[sessionID] = busy
    }

    /// Check if a session has backend busy state.
    func isBackendBusy(for sessionID: CanonicalConversationID) -> Bool {
        backendBusy[sessionID] ?? false
    }

    // MARK: - Connection state

    func updateConnectionState(_ state: GatewayClient.ConnectionState) {
        connectionState = state
    }

    // MARK: - Projection

    /// Get the current projection for a specific session.
    func projection(for sessionID: CanonicalConversationID) async -> TranscriptProjection? {
        guard let reducer = reducers[sessionID] else { return nil }
        return await reducer.projection(for: sessionID)
    }

    /// Update the foreground projection from the active session's reducer.
    private func updateForegroundProjection() async {
        guard let activeID = activeSessionID,
              let reducer = reducers[activeID] else {
            foregroundProjection = nil
            return
        }
        foregroundProjection = await reducer.projection(for: activeID)
    }

    // MARK: - Recovery journal

    func recordInFlight(
        sessionID: CanonicalConversationID,
        clientMessageID: ClientMessageID,
        visibleText: String,
        attachmentSignature: String?
    ) {
        recoveryJournal[sessionID] = RecoveryJournalEntry(
            clientMessageID: clientMessageID,
            visibleText: visibleText,
            attachmentSignature: attachmentSignature,
            recordedAt: Date()
        )
    }

    func clearRecoveryJournal(for sessionID: CanonicalConversationID) {
        recoveryJournal.removeValue(forKey: sessionID)
    }

    func recoveryEntry(for sessionID: CanonicalConversationID) -> RecoveryJournalEntry? {
        recoveryJournal[sessionID]
    }
}

// MARK: - Recovery Journal Entry

struct RecoveryJournalEntry: Sendable {
    let clientMessageID: ClientMessageID
    let visibleText: String
    let attachmentSignature: String?
    let recordedAt: Date
}
