import Foundation
@testable import Herald

// MARK: - Minimal mocks for ChatStore testing

@MainActor
final class MockHeraldClient: HeraldClientProtocol {
    var connectionStatus: ConnectionStatus = .disconnected
    var currentConversation: Conversation?

    func connect() async {}
    func disconnect() async {}
    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID) async -> Message {
        Message(sender: .user, content: message)
    }
    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID) -> AsyncStream<StreamingUpdate> {
        AsyncStream { $0.finish() }
    }
    func loadConversation() async -> Conversation { Conversation(id: UUID(), title: "") }
    func clearConversation() async throws -> Conversation { Conversation(id: UUID(), title: "") }
    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(id: UUID(), title: "") }
    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
        SessionListResponse(sessions: [], total: 0)
    }
    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
    func createSession(title: String) async throws -> SessionSummary {
        SessionSummary(title: title)
    }
    func deleteSession(id: UUID) async throws {}
    func archiveSession(id: UUID) async throws {}
    func togglePinSession(id: UUID) async throws -> SessionSummary {
        SessionSummary(title: "")
    }
    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        SessionSummary(title: title)
    }
    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "" }
    func loadConversation(id: UUID) async throws -> Conversation { Conversation(id: UUID(), title: "") }
    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        Message(sender: .user, content: text)
    }
    func cancelJob(jobID: UUID) async throws {}
}

@MainActor
final class MockPersistence: AppPersistenceStoreProtocol {
    var currentSessionId: UUID?
    func loadUserSettings() -> UserSettings? { nil }
    func saveUserSettings(_ settings: UserSettings) {}
    func loadSessionState() -> AppSessionState? { nil }
    func saveSessionState(_ state: AppSessionState) {}
    func clearSessionState() {}
    func loadInboxState() -> InboxLocalState { InboxLocalState() }
    func saveInboxState(_ state: InboxLocalState) {}
    func clearInboxState() {}
    func loadPairedRelayConfiguration() -> PairedRelayConfiguration? { nil }
    func savePairedRelayConfiguration(_ configuration: PairedRelayConfiguration) {}
    func clearPairedRelayConfiguration() {}
    func loadSensorOutboxState() -> SensorOutboxState { SensorOutboxState() }
    func saveSensorOutboxState(_ state: SensorOutboxState) {}
    func clearSensorOutboxState() {}
    func loadConversationCache() -> Conversation? { nil }
    func saveConversationCache(_ conversation: Conversation) {}
    func clearConversationCache() {}
    func loadHealthQueryAnchorData(for identifier: String) -> Data? { nil }
    func saveHealthQueryAnchorData(_ data: Data?, for identifier: String) {}
    func clearHealthQueryAnchorData() {}
    func loadSessionCache(key: String) -> [SessionSummary]? { nil }
    func saveSessionCache(_ sessions: [SessionSummary], key: String) {}
    func loadLogEntries() -> [LogEntry]? { nil }
    func saveLogEntries(_ entries: [LogEntry]) {}
}
