import Foundation

/// Response from a paginated session list request.
struct SessionListResponse: Codable, Sendable {
    let sessions: [SessionSummary]
    let total: Int
}

@MainActor
protocol HeraldClientProtocol {
    var connectionStatus: ConnectionStatus { get set }
    var currentConversation: Conversation? { get }
    func connect() async
    func disconnect() async
    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) async -> Message
    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) -> AsyncStream<StreamingUpdate>
    /// Send a message to a SPECIFIC conversation/session (used by note-as-session
    /// sync). Default implementation reports the capability is unavailable;
    /// the native gateway client overrides it with a real targeted send.
    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message
    /// Streaming variant of sendNoteMessage for note-as-session sync. Yields
    /// live reasoning deltas plus text, tool, and terminal events so the notes
    /// UI can render the agent's ACTUAL reasoning bubble (same component chat
    /// uses) while a note syncs. Default implementation reports the capability
    /// is unavailable; the native gateway client overrides it.
    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String?, enrichmentProvider: String?) -> AsyncStream<StreamingUpdate>

    /// Build 128.97: re-attach a note to its EXISTING gateway session by the
    /// FULL session key pinned on the note. Called before a note sync so an
    /// idMap wiped by a reinstall/container reset still resumes the same
    /// gateway session instead of spawning a duplicate. Default: no-op
    /// (legacy relay/mock clients have no durable key map).
    func resumeNoteSession(conversationID: UUID, sessionKey: String) async -> Bool

    /// Build 128.97: the FULL gateway session key currently mapped to a note's
    /// conversation UUID, if any. The sync engine pins this on the note after
    /// the first successful sync so future syncs can resume the same session.
    func nativeSessionKey(for conversationID: UUID) async -> String?
    func loadConversation() async -> Conversation
    func clearConversation() async throws -> Conversation
    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation

    // MARK: - Session Management

    /// List sessions with pagination.
    /// - Parameter allDevices: When true, includes sessions from every device on the account
    ///   instead of just this device's (+ user-scoped) sessions.
    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse

    /// Search sessions by query string.
    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary]

    /// Create a new session.
    func createSession(title: String) async throws -> SessionSummary

    /// Build 128: create a new session bound to a caller-minted conversation
    /// UUID. Used by SessionListStore.createNewSession so the local empty
    /// chat it installed via chatStore.installLocalConversation already
    /// matches the server-side session id — on the very next streaming
    /// send, idMap.nativeId(for: conversation.id) returns the freshly bound
    /// native id instead of minting a NEW one and orphaning the previous
    /// chat (the b127/b128 bug class).
    ///
    /// The native gateway uses this to register the caller's UUID (NOT a
    /// fresh random UUID) in its idMap. Default: delegates to the single-arg
    /// overload — mock and legacy relay clients ignore the conversationID.
    func createSession(title: String, conversationID: UUID?) async throws -> SessionSummary

    /// Delete a session by ID.
    func deleteSession(id: UUID) async throws

    /// Build 130.1: delete the gateway session backing a NOTE. The gateway's
    /// `session.delete` refuses ACTIVE sessions and matches DB rows by the
    /// FULL session key, so this closes the live session first (by its short
    /// native id) then deletes by the pinned FULL key. Best-effort: failures
    /// are logged by the caller, never fatal to the local note delete.
    /// Default: no-op for clients without a native gateway backing.
    func deleteNoteSession(conversationID: UUID, gatewaySessionKey: String?) async throws

    /// Archive a session by ID.
    func archiveSession(id: UUID) async throws

    /// Toggle pin state for a session.
    func togglePinSession(id: UUID) async throws -> SessionSummary

    /// Rename a session.
    func renameSession(id: UUID, title: String) async throws -> SessionSummary

    /// Generate a concise title via LLM for a session.
    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String

    /// Load a specific conversation by session ID.
    func loadConversation(id: UUID) async throws -> Conversation

    /// Build 31: ensure a server conversation exists for the given local UUID.
    /// Called before the first message so the connector can create a Hermes
    /// session and bind the mapping before the job runs.
    /// - Returns: `true` if the server session was created or already existed.
    func ensureConversation(id: UUID) async -> Bool

    /// Adopt a locally-minted conversation id as the active native
    /// conversation without going through the network. Used by ChatStore
    /// when it installs an empty local conversation immediately and binds
    /// the server session in the background, so the next streaming turn
    /// resolves the right session UUID without waiting for the round trip.
    /// Clients with a backing store (the native gateway) should also
    /// persist the mapping here if they maintain one, so a follow-up send
    /// in the same tick finds the local id already registered.
    func adoptConversation(id: UUID, title: String)

    /// Get the authoritative status of a job.
    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse?

    /// Reattach to a gateway session that is still running a job after the
    /// app was suspended (desktop parity: Electron calls session.resume on
    /// reconnect). Returns true when the session is live and still running,
    /// so the caller can keep the stream alive instead of declaring a stall.
    func resumeActiveSessionIfNeeded() async -> Bool
    /// Whether the server-side turn is parked waiting on the USER
    /// (a clarify question or approval prompt) rather than the model working.
    /// Server-turn watch uses this to tear down the synthetic Thinking bubble:
    /// when control has passed to the user, a spinner is a lie.
    func isServerTurnAwaitingUserInput() async -> Bool

    /// Build 128.76: fetch the full pending clarify payload (question +
    /// choices + request_id) for the current session, if the gateway parked
    /// the turn on one. Used on reconnect/refresh so the ClarifyCard can be
    /// re-shown after a missed live `clarify.request` event. Returns nil
    /// when no clarify is pending or the transport cannot probe.
    func fetchPendingClarify() async -> PendingClarify?

    /// Build 128.76: answer a clarify question the agent parked on. Sends
    /// `clarify.respond` with the request_id from the clarify.request event
    /// and the user's chosen answer so the gateway unblocks the turn.
    func respondToClarify(requestID: String, answer: String) async throws

    /// Build 84 Option C-A (probe-through-phantom): force a real liveness
    /// probe on the transport even when `connectionStatus == .connected`.
    /// A zombie socket (iOS suspended the WS in background, receive() never
    /// surfaced the error) reports connected but delivers nothing, which
    /// fools the stall watchdog into declaring a stall while the turn is
    /// still running server-side. The native client probes with the short
    /// session.list timeout and forces a fresh connect when the probe
    /// fails; on a live socket it returns immediately. The watchdog calls
    /// this before declaring a no-progress stall.
    func reconnectIfNeeded() async

    /// Returns the gateway session keys (e.g. 20260816_144338_2bbd59) that
    /// currently have an active turn in flight (status working). Used by the
    /// sidebar to render the blue "has action" dot. Clients without a native
    /// gateway connection return an empty set (default no-op).
    func activeSessionKeys() async -> Set<String>

    /// Send a message to a specific conversation with a specific client message ID.
    /// Used by notification actions where the target conversation may not be the current one.
    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message

    /// Cancel a running or queued job.
    func cancelJob(jobID: UUID) async throws

    /// Build 128.50: interrupt the CURRENT session's running turn server-side
    /// via session.interrupt. Unlike cancelJob (which needs the local jobID
    /// from activeStreams), this works when dropping into a session that is
    /// mid-turn on the server from another device - the app has no jobID for
    /// that turn, only the session. Returns true when the gateway accepted
    /// the interrupt.
    func interruptSession() async -> Bool

    /// Build 128.50: whether this client implementation can interrupt a
    /// server-side turn at all (relay path has no session.interrupt).
    var supportsServerTurnInterrupt: Bool { get }
}

// MARK: - Default implementations
extension HeraldClientProtocol {
    /// Default: no-op for clients that do not support session resume (mocks,
    /// legacy relay path). The native gateway client overrides this.
    func resumeActiveSessionIfNeeded() async -> Bool {
        false
    }

    /// Default: no-op for clients without a probeable transport (mocks,
    /// legacy relay path). The native gateway client overrides this with
    /// its 8s session.list probe + forced fresh connect.
    func reconnectIfNeeded() async {}

    /// Default: no active sessions for clients without a native gateway
    /// connection (mocks, legacy relay path). The native gateway client
    /// overrides this with a session.active_list probe.
    func activeSessionKeys() async -> Set<String> { [] }

    /// Default: no-op for clients without a clarify respond path (mocks,
    /// legacy relay path). The native gateway client overrides this to
    /// send clarify.respond over the WS.
    func respondToClarify(requestID: String, answer: String) async throws {}

    /// Default: no pending clarify for clients without a probeable transport
    /// (mocks, legacy relay path). The native gateway client overrides this
    /// to decode pending_clarify from the session.resume payload.
    func fetchPendingClarify() async -> PendingClarify? { nil }

    /// Default: no-op for clients without a backing idMap (mocks, legacy
    /// relay path). The native gateway client overrides this to re-point
    /// `currentConversation` and register the local UUID so the next send
    /// resolves the right session without a redundant transcript download.
    func adoptConversation(id: UUID, title: String) {}

    /// Default: delegates to the single-arg overload so conformances that
    /// pre-date the b128 conversation-binding change keep working. The
    /// native gateway client overrides this to bind the caller's UUID.
    func createSession(title: String, conversationID: UUID?) async throws -> SessionSummary {
        try await createSession(title: title)
    }

    /// Default: no-op for clients without a durable key map (mocks, legacy
    /// relay path). The native gateway client overrides this with
    /// session.resume + idMap re-registration.
    func resumeNoteSession(conversationID: UUID, sessionKey: String) async -> Bool { false }

    /// Default: no-op for clients without a native gateway backing (mocks,
    /// legacy relay path). The native gateway client overrides this with the
    /// close-then-delete sequence.
    func deleteNoteSession(conversationID: UUID, gatewaySessionKey: String?) async throws {}

    /// Default: nil for clients without a durable key map (mocks, legacy
    /// relay path). The native gateway client overrides this to read its
    /// idMap's FULL-key map.
    func nativeSessionKey(for conversationID: UUID) async -> String? { nil }

    /// Default: no-op for clients without session.interrupt (mocks, legacy
    /// relay path). The native gateway client overrides this with the
    /// real gateway call.
    func interruptSession() async -> Bool { false }

    /// Default: assume the server turn is NOT parked waiting on the user.
    /// Only transports that can decode pending_clarify/pending_approval
    /// (native gateway) override this.
    func isServerTurnAwaitingUserInput() async -> Bool { false }

    /// Default: only the native gateway client can interrupt a server-side
    /// turn; relay/mock clients return false so the UI falls back to local
    /// teardown only.
    var supportsServerTurnInterrupt: Bool { false }
}


// MARK: - Default sendNoteMessage (unavailable for non-native clients)

extension HeraldClientProtocol {
    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message {
        Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .herald,
            content: "Note sync requires the native gateway client.",
            status: .failed
        )
    }

    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String? = nil, enrichmentProvider: String? = nil) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            continuation.yield(.failed("Note sync requires the native gateway client."))
            continuation.finish()
        }
    }
}

