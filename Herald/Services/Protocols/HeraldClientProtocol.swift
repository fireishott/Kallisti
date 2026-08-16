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
}
