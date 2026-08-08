import Foundation
import os

/// Maps between the app's UUID-based session model and the native gateway's
/// short-hex session_id model. Stores the mapping in UserDefaults.
actor NativeSessionIdMap {
    private static let defaultsKey = "NativeGwSessionIdMap"
    private var uuidToNative: [String: String] = [:]
    private var nativeToUuid: [String: String] = [:]

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.defaultsKey),
           let map = try? JSONDecoder().decode([String: String].self, from: data) {
            uuidToNative = map
            for (k, v) in map { nativeToUuid[v] = k }
        }
    }

    func register(uuid: UUID, nativeId: String) {
        uuidToNative[uuid.uuidString] = nativeId
        nativeToUuid[nativeId] = uuid.uuidString
        save()
    }

    func nativeId(for uuid: UUID) -> String? {
        uuidToNative[uuid.uuidString]
    }

    func uuid(for nativeId: String) -> UUID? {
        nativeToUuid[nativeId].flatMap(UUID.init)
    }

    func remove(uuid: UUID) {
        if let native = uuidToNative.removeValue(forKey: uuid.uuidString) {
            nativeToUuid.removeValue(forKey: native)
            save()
        }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(uuidToNative) {
            UserDefaults.standard.set(data, forKey: Self.defaultsKey)
        }
    }
}

/// NativeHeraldClient: implements HeraldClientProtocol using the native
/// JSON-RPC/WebSocket gateway (ws://host:9119/api/ws) instead of the
/// REST+SSE connector facade.
///
/// This is the 0.2.0 path. Session IDs are mapped between UUID (app side)
/// and short-hex (gateway side) via NativeSessionIdMap.
@MainActor
final class NativeHeraldClient: HeraldClientProtocol {
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "NativeHeraldClient")

    // MARK: - Dependencies

    private let gatewayBaseURL: String
    private let credentials: NativeAuthCoordinator.Credentials
    private let idMap = NativeSessionIdMap()
    private var client: NativeGatewayClient?
    private var transport: URLSessionWebSocketTransport?

    // MARK: - HeraldClientProtocol

    var connectionStatus: ConnectionStatus = .disconnected
    var currentConversation: Conversation?

    init(gatewayBaseURL: String, credentials: NativeAuthCoordinator.Credentials) {
        self.gatewayBaseURL = gatewayBaseURL
        self.credentials = credentials
    }

    func connect() async {
        connectionStatus = .connecting
        do {
            let wsURL = try await NativeAuthCoordinator.authenticatedWSURL(
                baseURL: gatewayBaseURL,
                credentials: credentials
            )
            let transport = URLSessionWebSocketTransport()
            let client = NativeGatewayClient(transport: transport)

            try await client.connect(url: wsURL)
            self.transport = transport
            self.client = client
            connectionStatus = .connected
            Self.logger.info("Connected to native gateway")
        } catch {
            connectionStatus = .disconnected
            Self.logger.error("Failed to connect: \(error)")
        }
    }

    func disconnect() async {
        await client?.close()
        client = nil
        transport = nil
        connectionStatus = .disconnected
    }

    // MARK: - Session Lifecycle

    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let params = SessionListParams(limit: limit, offset: offset)
        let response = try await client.send(method: "session.list", params: params)
        if let error = response.error { throw error }
        guard let result = response.result else {
            return SessionListResponse(sessions: [], total: 0)
        }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionListResult.self, from: data)

        var sessions: [SessionSummary] = []
        for native in decoded.sessions {
            let uuid = await idMap.uuid(for: native.sessionId) ?? UUID()
            sessions.append(SessionSummary(
                id: uuid,
                title: native.title ?? "Untitled",
                previewText: native.previewText ?? "",
                lastActivity: native.lastActivity.flatMap { ISO8601DateFormatter().date(from: $0) } ?? .now,
                isPinned: native.isPinned ?? false,
                isArchived: native.isArchived ?? false
            ))
        }
        return SessionListResponse(sessions: sessions, total: decoded.total ?? sessions.count)
    }

    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "session.search", params: ["query": query])
        if let error = response.error { throw error }
        guard let result = response.result else { return [] }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionListResult.self, from: data)
        return decoded.sessions.map { native in
            SessionSummary(
                title: native.title ?? "Untitled",
                previewText: native.previewText ?? ""
            )
        }
    }

    func createSession(title: String) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "session.create", params: ["title": title])
        if let error = response.error { throw error }
        guard let result = response.result else { throw NativeGatewayClientError.unexpectedFrame }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionCreateResult.self, from: data)
        let uuid = UUID()
        await idMap.register(uuid: uuid, nativeId: decoded.sessionId)
        return SessionSummary(id: uuid, title: title, lastActivity: .now)
    }

    func deleteSession(id: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.delete", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        await idMap.remove(uuid: id)
    }

    func archiveSession(id: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.archive", params: ["session_id": nativeId])
        if let error = response.error { throw error }
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.toggle_pin", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        return SessionSummary(id: id, title: "")
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.rename", params: ["session_id": nativeId, "title": title])
        if let error = response.error { throw error }
        return SessionSummary(id: id, title: title)
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        return ""
    }

    // MARK: - Conversation

    func loadConversation() async -> Conversation {
        let conv = Conversation(title: "New Chat")
        currentConversation = conv
        return conv
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.history", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        guard let result = response.result else { return Conversation(id: id, title: "Untitled") }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionHistoryResult.self, from: data)
        let messages = decoded.messages.map { msg -> Message in
            Message(
                id: UUID(),
                sender: msg.role == "assistant" ? .herald : .user,
                content: msg.content,
                timestamp: msg.timestamp.flatMap { ISO8601DateFormatter().date(from: $0) } ?? .now
            )
        }
        let conv = Conversation(id: id, title: decoded.title ?? "Untitled", messages: messages)
        currentConversation = conv
        return conv
    }

    func clearConversation() async throws -> Conversation {
        let conv = Conversation(title: "New Chat")
        currentConversation = conv
        return conv
    }

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        return currentConversation ?? Conversation(title: "New Chat")
    }

    func ensureConversation(id: UUID) async -> Bool {
        if await idMap.nativeId(for: id) != nil { return true }
        do {
            _ = try await createSession(title: "New Chat")
            return true
        } catch {
            return false
        }
    }

    // MARK: - Send / Streaming

    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) async -> Message {
        var finalContent = ""
        var finalUsage: TokenUsage?
        for await update in sendStreaming(message: message, attachments: attachments, clientMessageID: clientMessageID, continuationContext: continuationContext) {
            switch update {
            case .textDelta(let text):
                finalContent += text
            case .finished(let msg, let usage, _, _):
                return msg
            case .failed(let error, _, _):
                return Message(id: clientMessageID, sender: .herald, content: "Error: \(error)", timestamp: .now)
            default:
                break
            }
        }
        return Message(id: clientMessageID, sender: .herald, content: finalContent, timestamp: .now)
    }

    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            Task { [weak self] in
                guard let self else { continuation.finish(); return }
                await self._sendStreaming(
                    message: message,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
            }
        }
    }

    private func _sendStreaming(
        message: String,
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        guard let client else {
            continuation.yield(.failed("Not connected"))
            continuation.finish()
            return
        }

        // Get or create session
        let sessionUUID = currentConversation?.id ?? UUID()
        var nativeSessionId = await idMap.nativeId(for: sessionUUID)

        if nativeSessionId == nil {
            do {
                let summary = try await createSession(title: "New Chat")
                nativeSessionId = await idMap.nativeId(for: summary.id)
            } catch {
                continuation.yield(.failed("Failed to create session: \(error)"))
                continuation.finish()
                return
            }
        }

        guard let sid = nativeSessionId else {
            continuation.yield(.failed("No session ID"))
            continuation.finish()
            return
        }

        // Register a one-shot event handler for this stream
        let handler = StreamEventHandler(sessionId: sid, continuation: continuation)
        await client.onEvent { event in
            handler.handle(event)
        }

        // Submit the prompt
        do {
            let response = try await client.send(
                method: "prompt.submit",
                params: PromptSubmitParams(sessionId: sid, text: message)
            )
            if let error = response.error {
                continuation.yield(.failed(error.message))
                continuation.finish()
                return
            }
        } catch {
            continuation.yield(.failed("Submit failed: \(error)"))
            continuation.finish()
            return
        }
    }

    func cancelJob(jobID: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "prompt.cancel", params: ["job_id": jobID.uuidString])
        if let error = response.error { throw error }
    }

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        return await send(message: text, attachments: [], clientMessageID: clientMessageID, continuationContext: nil)
    }
}

// MARK: - Stream Event Handler

/// Processes gateway events for a single streaming turn.
private final class StreamEventHandler: @unchecked Sendable {
    let sessionId: String
    let continuation: AsyncStream<StreamingUpdate>.Continuation
    private var completed = false

    init(sessionId: String, continuation: AsyncStream<StreamingUpdate>.Continuation) {
        self.sessionId = sessionId
        self.continuation = continuation
    }

    func handle(_ event: NativeGatewayEvent) {
        guard event.params.sessionId == sessionId, !completed else { return }
        switch event.params.type {
        case "message.delta":
            if let delta = event.params.decodePayload(NativeMessageDeltaPayload.self) {
                continuation.yield(.textDelta(delta.text))
            }
        case "thinking.delta":
            if let delta = event.params.decodePayload(NativeThinkingDeltaPayload.self) {
                continuation.yield(.reasoningDelta(delta.text))
            }
        case "message.complete":
            if let complete = event.params.decodePayload(NativeMessageCompletePayload.self) {
                let usage = complete.usage.map { u in
                    TokenUsage(
                        promptTokens: u.input ?? 0,
                        completionTokens: u.output ?? 0,
                        totalTokens: u.total ?? 0
                    )
                }
                let msg = Message(
                    id: UUID(),
                    sender: .herald,
                    content: complete.text ?? "",
                    timestamp: .now
                )
                continuation.yield(.finished(msg, usage, nil, nil))
                completed = true
                continuation.finish()
            }
        default:
            break
        }
    }
}

// MARK: - Encodable param structs

private struct SessionListParams: Encodable {
    let limit: Int
    let offset: Int
}

private struct PromptSubmitParams: Encodable {
    let sessionId: String
    let text: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case text
    }
}
