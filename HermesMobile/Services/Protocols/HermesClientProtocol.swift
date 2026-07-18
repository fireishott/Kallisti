import Foundation

@MainActor
protocol HermesClientProtocol {
    var connectionStatus: ConnectionStatus { get }
    var currentConversation: Conversation? { get }
    func connect() async
    func disconnect() async
    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message
    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate>
    func loadConversation() async -> Conversation
    func clearConversation() async throws -> Conversation
    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation

    // MARK: - Session Management

    func listSessions(limit: Int, offset: Int) async -> [SessionSummary]
    func loadSession(id: UUID) async -> Conversation
    func createSession(title: String?) async -> Conversation
    func renameSession(id: UUID, title: String) async
    func archiveSession(id: UUID) async
    func pinSession(id: UUID) async
    func unpinSession(id: UUID) async
    func deleteSession(id: UUID) async
    func setActiveSession(id: UUID) async
    func searchSessions(query: String) async -> [SessionSummary]
}
