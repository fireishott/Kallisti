import Foundation

/// Response from a paginated session list request.
struct SessionListResponse: Codable, Sendable {
    let sessions: [SessionSummary]
    let total: Int
}

/// T1.6: result of ensureConversation — carries the server's canonical
/// conversation id when the client presented a derived alias.
struct EnsureResult: Sendable {
    /// Whether the session was established (server returned a session).
    let ok: Bool
    /// The server's canonical conversation id, if different from the
    /// one the client requested (v5 alias → v4 canonical redirect).
    let canonicalID: UUID?
    /// Server-provided human-readable message (e.g. binding conflict reason).
    let serverMessage: String?
}

@MainActor
protocol KallistiClientProtocol {
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
    /// - Returns: `EnsureResult` — `ok` if the server session was created
    ///   or already existed; `canonicalID` when the server redirected from
    ///   a derived alias to the real conversation id.
    func ensureConversation(id: UUID) async -> EnsureResult

    /// Get the authoritative status of a job.
    func getJobStatus(_ jobId: UUID) async -> LiveKallistiClient.JobStatusResponse?

    /// Send a message to a specific conversation with a specific client message ID.
    /// Used by notification actions where the target conversation may not be the current one.
    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message

    /// Cancel a running or queued job.
    func cancelJob(jobID: UUID) async throws
}
