import Foundation

@MainActor
final class ResilientHeraldClient: HeraldClientProtocol {
    var connectionStatus: ConnectionStatus {
        get { primary.connectionStatus }
        set { primary.connectionStatus = newValue }
    }

    var currentConversation: Conversation? {
        primary.currentConversation ?? fallback.currentConversation
    }

    private var primary: any HeraldClientProtocol
    private let fallback: any HeraldClientProtocol
    private let allowsFallback: @MainActor () -> Bool

    init(
        primary: any HeraldClientProtocol,
        fallback: any HeraldClientProtocol,
        allowsFallback: @escaping @MainActor () -> Bool = { true }
    ) {
        self.primary = primary
        self.fallback = fallback
        self.allowsFallback = allowsFallback
    }

    func connect() async {
        await primary.connect()
        if allowsFallback() && primary.connectionStatus == .error {
            await fallback.connect()
        }
    }

    func disconnect() async {
        await primary.disconnect()
        await fallback.disconnect()
    }

    func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) async -> Message {
        let response = await primary.send(message: message, attachments: attachments, clientMessageID: clientMessageID)
        if allowsFallback() && response.status == .failed {
            return await fallback.send(message: message, attachments: attachments, clientMessageID: clientMessageID)
        }
        return response
    }

    func sendStreaming(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
        primary.sendStreaming(message: message, attachments: attachments, clientMessageID: clientMessageID)
    }

    func loadConversation() async -> Conversation {
        let conversation = await primary.loadConversation()
        if allowsFallback() && primary.connectionStatus == .error {
            return await fallback.loadConversation()
        }
        return conversation
    }

    func clearConversation() async throws -> Conversation {
        try await primary.clearConversation()
    }

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        try await primary.injectVoiceTranscript(voiceSessionId: voiceSessionId)
    }
}

// MARK: - Session Management

extension ResilientHeraldClient {
    func listSessions(limit: Int, offset: Int, allDevices: Bool = false) async throws -> SessionListResponse {
        do {
            return try await primary.listSessions(limit: limit, offset: offset, allDevices: allDevices)
        } catch {
            guard allowsFallback() else { throw error }
            return try await fallback.listSessions(limit: limit, offset: offset, allDevices: allDevices)
        }
    }

    func searchSessions(query: String, allDevices: Bool = false) async throws -> [SessionSummary] {
        do {
            return try await primary.searchSessions(query: query, allDevices: allDevices)
        } catch {
            guard allowsFallback() else { throw error }
            return try await fallback.searchSessions(query: query, allDevices: allDevices)
        }
    }

    func createSession(title: String) async throws -> SessionSummary {
        try await primary.createSession(title: title)
    }

    func deleteSession(id: UUID) async throws {
        try await primary.deleteSession(id: id)
    }

    func archiveSession(id: UUID) async throws {
        try await primary.archiveSession(id: id)
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        try await primary.togglePinSession(id: id)
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        try await primary.renameSession(id: id, title: title)
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        try await primary.generateSessionTitle(sessionId: sessionId, userMessage: userMessage, assistantMessage: assistantMessage)
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        try await primary.loadConversation(id: id)
    }

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
        await primary.getJobStatus(jobId)
    }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        try await primary.sendMessage(text, conversationID: conversationID, clientMessageID: clientMessageID)
    }

    func cancelJob(jobID: UUID) async throws {
        try await primary.cancelJob(jobID: jobID)
    }
}
