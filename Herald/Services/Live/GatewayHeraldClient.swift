import Foundation
import os

/// HeraldClientProtocol implementation using JSON-RPC 2.0 over WebSocket.
///
/// This client communicates with the relay's `/api/ws` endpoint using the
/// same protocol as Hermes Desktop. It replaces the REST/SSE path used
/// by `LiveHeraldClient`.
@MainActor
final class GatewayHeraldClient: HeraldClientProtocol {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "GatewayHeraldClient")

    var connectionStatus: ConnectionStatus = .disconnected
    var currentConversation: Conversation?
    var reasoningEffortProvider: (() -> String)?

    private var gatewayClient: GatewayClient?
    private let projectionStore = SessionProjectionStore()
    private var eventRouter: GatewayEventRouter?
    private let accessTokenProvider: () async -> String?
    private let relayBaseURLProvider: () -> String?

    init(
        relayBaseURLProvider: @escaping () -> String?,
        accessTokenProvider: @escaping () async -> String?
    ) {
        self.relayBaseURLProvider = relayBaseURLProvider
        self.accessTokenProvider = accessTokenProvider
    }

    // MARK: - Connection

    func connect() async {
        connectionStatus = .connecting

        let token = await accessTokenProvider()
        let relayURL = relayBaseURLProvider() ?? ""

        guard let config = GatewayClient.Configuration.from(relayBaseURL: relayURL, authToken: token) else {
            Self.logger.error("Invalid relay URL: \(relayURL)")
            connectionStatus = .disconnected
            return
        }

        let client = GatewayClient(config: config)
        self.gatewayClient = client
        self.eventRouter = GatewayEventRouter(gatewayClient: client, projectionStore: projectionStore)

        do {
            try await client.connect()
            connectionStatus = .connected
            await eventRouter?.startRouting()
            Self.logger.info("Connected to gateway via JSON-RPC WebSocket")
        } catch {
            connectionStatus = .disconnected
            Self.logger.error("Failed to connect: \(error.localizedDescription)")
        }
    }

    func disconnect() async {
        await eventRouter?.stopRouting()
        await gatewayClient?.disconnect()
        gatewayClient = nil
        eventRouter = nil
        connectionStatus = .disconnected
    }

    // MARK: - Chat

    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) async -> Message {
        var lastMessage = Message(sender: .herald, content: "", status: .failed)

        for await update in sendStreaming(message: message, attachments: attachments, clientMessageID: clientMessageID, continuationContext: continuationContext) {
            if case .finished(let msg, _, _, _) = update {
                lastMessage = msg
            }
        }

        return lastMessage
    }

    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) -> AsyncStream<StreamingUpdate> {
        let (stream, continuation) = AsyncStream<StreamingUpdate>.makeStream()

        Task { @MainActor [weak self] in
            guard let self, let gatewayClient = self.gatewayClient else {
                continuation.yield(.failed("Not connected"))
                continuation.finish()
                return
            }

            guard let conversationID = self.currentConversation?.id.uuidString else {
                continuation.yield(.failed("No active conversation"))
                continuation.finish()
                return
            }

            // Apply optimistic user to projection
            if let sessionID = CanonicalConversationID(conversationID) {
                let submission = TranscriptReducer.OptimisticUserSubmission(
                    conversationID: sessionID,
                    clientMessageID: ClientMessageID(clientMessageID.uuidString)!,
                    displayContent: message
                )
                await self.projectionStore.applyOptimisticUser(submission, epoch: await self.projectionStore.currentEpoch)
            }

            let params: [String: Any] = [
                "session_id": conversationID,
                "message": message,
                "client_message_id": clientMessageID.uuidString
            ]

            do {
                let response = try await gatewayClient.request(method: "prompt.submit", params: params)

                if let error = response.error {
                    continuation.yield(.failed(error.message))
                    continuation.finish()
                    return
                }

                // Extract job_id from response if available
                let jobID = UUID() // Placeholder - should come from response
                continuation.yield(.messageSent(jobID: jobID))
                continuation.finish()

            } catch {
                continuation.yield(.failed(error.localizedDescription))
                continuation.finish()
            }
        }

        return stream
    }

    // MARK: - Conversation

    func loadConversation() async -> Conversation {
        let conversation = Conversation(id: UUID(), title: "New Chat")
        currentConversation = conversation
        return conversation
    }

    func clearConversation() async throws -> Conversation {
        let conversation = Conversation(id: UUID(), title: "New Chat")
        currentConversation = conversation
        return conversation
    }

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        throw ClientError.notSupported
    }

    // MARK: - Session Management

    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
        guard let gatewayClient else { throw ClientError.notConnected }

        let params: [String: Any] = ["limit": limit, "offset": offset]
        let response = try await gatewayClient.request(method: "session.list", params: params)

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }

        return SessionListResponse(sessions: [], total: 0)
    }

    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
        guard let gatewayClient else { throw ClientError.notConnected }

        let response = try await gatewayClient.request(method: "session.search", params: ["query": query])

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }

        return []
    }

    func createSession(title: String) async throws -> SessionSummary {
        guard let gatewayClient else { throw ClientError.notConnected }

        let response = try await gatewayClient.request(method: "session.create", params: ["title": title])

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }

        return SessionSummary(title: title)
    }

    func deleteSession(id: UUID) async throws {
        guard let gatewayClient else { throw ClientError.notConnected }

        let response = try await gatewayClient.request(method: "session.delete", params: ["session_id": id.uuidString])

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }
    }

    func archiveSession(id: UUID) async throws {
        guard let gatewayClient else { throw ClientError.notConnected }

        let response = try await gatewayClient.request(method: "session.archive", params: ["session_id": id.uuidString])

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        throw ClientError.notSupported
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        guard let gatewayClient else { throw ClientError.notConnected }

        let params: [String: Any] = ["session_id": id.uuidString, "title": title]
        let response = try await gatewayClient.request(method: "session.rename", params: params)

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }

        return SessionSummary(id: id, title: title)
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        throw ClientError.notSupported
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let gatewayClient else { throw ClientError.notConnected }

        let response = try await gatewayClient.request(method: "session.messages", params: ["session_id": id.uuidString])

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }

        let conversation = Conversation(id: id, title: "")
        currentConversation = conversation
        return conversation
    }

    func ensureConversation(id: UUID) async -> Bool {
        do {
            _ = try await createSession(title: "New Chat")
            return true
        } catch {
            return false
        }
    }

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }

    func cancelJob(jobID: UUID) async throws {
        guard let gatewayClient else { throw ClientError.notConnected }

        let response = try await gatewayClient.request(method: "prompt.cancel", params: ["session_id": currentConversation?.id.uuidString ?? ""])

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }
    }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        guard let gatewayClient else { throw ClientError.notConnected }

        let params: [String: Any] = [
            "session_id": conversationID.uuidString,
            "message": text,
            "client_message_id": clientMessageID.uuidString
        ]

        let response = try await gatewayClient.request(method: "prompt.submit", params: params)

        if let error = response.error {
            throw ClientError.serverError(error.message)
        }

        return Message(sender: .user, content: text, status: .sent)
    }

    // MARK: - Model/Profile

    func listModels() async throws -> [ModelInfo] { [] }
    func switchModel(name: String) async throws {}
    func listProfiles() async throws -> [ProfileInfo] { [] }

    // MARK: - Gateway Control

    func getGatewayStatus() async throws -> GatewayStatusResponse {
        GatewayStatusResponse(connected: true, activeJobs: 0)
    }

    func restartGateway() async throws {}
    func getGatewayLogs(limit: Int) async throws -> [GatewayLogEntry] { [] }
}

// MARK: - Supporting Types

enum ClientError: Error, LocalizedError {
    case noConversation
    case notConnected
    case serverError(String)
    case timeout
    case notSupported

    var errorDescription: String? {
        switch self {
        case .noConversation: return "No active conversation"
        case .notConnected: return "Not connected to gateway"
        case .serverError(let msg): return "Server error: \(msg)"
        case .timeout: return "Request timed out"
        case .notSupported: return "Operation not supported"
        }
    }
}

struct ModelInfo: Codable, Sendable { let name: String; let active: Bool }
struct ProfileInfo: Codable, Sendable { let name: String; let active: Bool }
struct GatewayStatusResponse: Codable, Sendable { let connected: Bool; let activeJobs: Int }
struct GatewayLogEntry: Codable, Sendable { let timestamp: Date; let level: String; let message: String }
