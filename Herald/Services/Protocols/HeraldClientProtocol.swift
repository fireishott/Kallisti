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

    /// Get the authoritative status of a job.
    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse?

    /// Reattach to a gateway session that is still running a job after the
    /// app was suspended (desktop parity: Electron calls session.resume on
    /// reconnect). Returns true when the session is live and still running,
    /// so the caller can keep the stream alive instead of declaring a stall.
    func resumeActiveSessionIfNeeded() async -> Bool

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
}
