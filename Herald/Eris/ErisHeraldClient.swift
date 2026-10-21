import Foundation

// MARK: - Eris-backed Herald client
//
// The one `HeraldClientProtocol` implementation the 0.4.0 stores call. Backed by
// the Eris device transport (`ErisClient`). Members Eris has no route for throw
// a typed `ErisHeraldError.notBackedYet` so a store's error path handles it -
// never a crash and never a silently faked success.

enum ErisHeraldError: Error, LocalizedError {
    case notBackedYet(String)
    case notConnected

    var errorDescription: String? {
        switch self {
        case .notBackedYet(let feature):
            return "\(feature) is not backed by Eris yet."
        case .notConnected:
            return "No Eris daemon is paired."
        }
    }
}

@MainActor
final class ErisHeraldClient: HeraldClientProtocol {
    var connectionStatus: ConnectionStatus = .disconnected
    private(set) var currentConversation: Conversation?

    /// Owns pairing and the sensor pipeline; the transport is pulled from it.
    private let store = ErisDaemonStore()

    /// Local conversation UUID <-> Eris conversation id (opaque string).
    private var conversationIDByLocalID: [UUID: String] = [:]
    private var localIDByConversationID: [String: UUID] = [:]

    // MARK: - Connection

    func connect() async {
        connectionStatus = .connecting
        await store.start()
        connectionStatus = store.pairedBaseURL != nil ? .connected : .disconnected
    }

    func disconnect() async {
        connectionStatus = .disconnected
        currentConversation = nil
    }

    // MARK: - Turns

    func send(
        message: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        continuationContext: String?
    ) async -> Message {
        var final: Message?
        for await update in sendStreaming(
            message: message,
            attachments: attachments,
            clientMessageID: clientMessageID,
            continuationContext: continuationContext
        ) {
            if case .finished(let message, _, _, _) = update { final = message }
        }
        if let final {
            return final
        }
        return Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .herald,
            content: "No reply received.",
            status: .failed
        )
    }

    func sendStreaming(
        message: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        continuationContext: String?
    ) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            let task = Task { @MainActor [weak self] in
                await self?.performStreamingSend(
                    message: message,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func performStreamingSend(
        message: String,
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        guard let client = store.clientForPairedDaemon() else {
            continuation.yield(.failed("No Eris daemon is paired."))
            continuation.finish()
            return
        }

        do {
            // Eris turns carry text only; attachments have no route yet and are
            // dropped (the composer's text still reaches the model).
            let turn = try await client.createTurn(
                text: message,
                conversation: currentConversationID()
            )
            let jobID = Self.stableUUID(from: turn.jobID)
            continuation.yield(.messageSent(jobID: jobID))

            var text = ""
            let stream = await client.events(turnID: turn.jobID)
            for try await frame in stream {
                switch frame {
                case .delta(let delta):
                    text += delta
                    continuation.yield(.textDelta(delta))
                case .done(let full, let usage):
                    let content = full.isEmpty ? text : full
                    let final = Message(
                        id: clientMessageID,
                        clientMessageID: clientMessageID,
                        sender: .herald,
                        content: content,
                        jobID: jobID,
                        status: .sent
                    )
                    let tokenUsage = usage.map {
                        TokenUsage(
                            promptTokens: $0.inputTokens ?? 0,
                            completionTokens: $0.outputTokens ?? 0,
                            totalTokens: ($0.inputTokens ?? 0) + ($0.outputTokens ?? 0)
                        )
                    }
                    continuation.yield(.finished(final, tokenUsage, nil, nil))
                    continuation.finish()
                    return
                case .error(let error):
                    continuation.yield(.failed(error.errorDescription ?? "turn failed", category: error.code))
                    continuation.finish()
                    return
                case .cancelled:
                    continuation.yield(.cancelled)
                    continuation.finish()
                    return
                }
            }
            continuation.finish()
        } catch {
            continuation.yield(.failed((error as? ErisError)?.errorDescription ?? error.localizedDescription))
            continuation.finish()
        }
    }

    // MARK: - Conversation (transcript)

    func loadConversation() async -> Conversation {
        if let currentConversation {
            return currentConversation
        }
        // No conversation bound yet: hand back an empty one the chat can use.
        let conversation = Conversation(title: "New Chat")
        currentConversation = conversation
        return conversation
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let client = store.clientForPairedDaemon() else { throw ErisHeraldError.notConnected }
        guard let conversationID = conversationIDByLocalID[id] else {
            return Conversation(id: id, title: "New Chat")
        }
        let detail = try await client.conversation(id: conversationID)
        let conversation = Conversation(
            id: id,
            title: detail.displayTitle,
            messages: detail.messages.map { Self.message(from: $0) },
            sessionKey: conversationID
        )
        currentConversation = conversation
        return conversation
    }

    func clearConversation() async throws -> Conversation {
        guard let client = store.clientForPairedDaemon() else { throw ErisHeraldError.notConnected }
        if let conversationID = currentConversationID() {
            _ = try await client.clearConversation(id: conversationID)
        }
        let conversation = Conversation(title: "New Chat")
        currentConversation = conversation
        return conversation
    }

    func ensureConversation(id: UUID) async -> Bool {
        if conversationIDByLocalID[id] != nil { return true }
        guard let client = store.clientForPairedDaemon() else { return false }
        do {
            let created = try await client.createConversation()
            conversationIDByLocalID[id] = created.id
            localIDByConversationID[created.id] = id
            store.noteSession(created.id)
            return true
        } catch {
            return false
        }
    }

    func adoptConversation(id: UUID, title: String) {
        let conversation = Conversation(id: id, title: title)
        currentConversation = conversation
        _ = conversationIDByLocalID[id] // keep the id map entry if present
    }

    // MARK: - Sessions (conversations)

    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
        guard let client = store.clientForPairedDaemon() else { throw ErisHeraldError.notConnected }
        let page = try await client.conversations()
        let sessions = page.conversations.map { Self.summary(from: $0) }
        return SessionListResponse(sessions: sessions, total: sessions.count)
    }

    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
        let response = try await listSessions(limit: 100, offset: 0, allDevices: allDevices)
        guard !query.isEmpty else { return response.sessions }
        return response.sessions.filter {
            $0.title.localizedCaseInsensitiveContains(query)
        }
    }

    func createSession(title: String) async throws -> SessionSummary {
        guard let client = store.clientForPairedDaemon() else { throw ErisHeraldError.notConnected }
        let created = try await client.createConversation()
        let localID = localID(forConversation: created.id)
        return SessionSummary(
            id: localID,
            title: created.title.isEmpty ? "New Chat" : created.title,
            sessionKey: created.id
        )
    }

    func deleteSession(id: UUID) async throws {
        // Eris has no delete-conversation route (only clear). Fail typed.
        throw ErisHeraldError.notBackedYet("deleteSession")
    }

    func archiveSession(id: UUID) async throws {
        throw ErisHeraldError.notBackedYet("archiveSession")
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        throw ErisHeraldError.notBackedYet("togglePinSession")
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        throw ErisHeraldError.notBackedYet("renameSession")
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        throw ErisHeraldError.notBackedYet("generateSessionTitle")
    }

    func generateCreativeTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        throw ErisHeraldError.notBackedYet("generateCreativeTitle")
    }

    // MARK: - Jobs

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
        nil
    }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        guard let client = store.clientForPairedDaemon() else { throw ErisHeraldError.notConnected }
        let erisConversationID = conversationIDByLocalID[conversationID]
        let turn = try await client.createTurn(text: text, conversation: erisConversationID)
        // Drain the stream to completion to collect the final text.
        var text = ""
        let stream = await client.events(turnID: turn.jobID)
        for try await frame in stream {
            if case .done(let full, _) = frame { text = full; break }
            if case .delta(let d) = frame { text += d }
            if case .error(let e) = frame { throw e }
        }
        return Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .herald,
            content: text,
            jobID: Self.stableUUID(from: turn.jobID),
            status: .sent
        )
    }

    func cancelJob(jobID: UUID) async throws {
        throw ErisHeraldError.notBackedYet("cancelJob (needs the Eris turn id)")
    }

    func interruptSession() async -> Bool {
        false
    }

    // MARK: - Notes (not the chat path; notes are handled by NotesStore)

    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message {
        Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .herald,
            content: "Note sync is not backed by Eris yet.",
            status: .failed
        )
    }

    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String?, enrichmentProvider: String?, thinkingAsReasoning: Bool) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            continuation.yield(.failed("Note sync is not backed by Eris yet."))
            continuation.finish()
        }
    }

    // MARK: - Voice

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        throw ErisHeraldError.notBackedYet("injectVoiceTranscript")
    }

    // MARK: - Helpers

    private func currentConversationID() -> String? {
        guard let currentConversation else { return nil }
        return conversationIDByLocalID[currentConversation.id]
    }

    private func localID(forConversation conversationID: String) -> UUID {
        if let existing = localIDByConversationID[conversationID] { return existing }
        let local = UUID()
        localIDByConversationID[conversationID] = local
        conversationIDByLocalID[local] = conversationID
        return local
    }

    /// A deterministic UUID for an opaque Eris id (job/session/conversation).
    static func stableUUID(from string: String) -> UUID {
        if let uuid = UUID(uuidString: string) { return uuid }
        var hash: UInt64 = 0xcbf29ce484222325
        for byte in string.utf8 {
            hash ^= UInt64(byte)
            hash &*= 0x100000001b3
        }
        var bytes = [UInt8](repeating: 0, count: 16)
        withUnsafeBytes(of: hash.littleEndian) { buf in
            for i in 0..<8 { bytes[i] = buf[i] }
        }
        var hash2: UInt64 = 0x84222325cbf29ce4 ^ UInt64(string.count)
        for byte in string.utf8.reversed() {
            hash2 ^= UInt64(byte)
            hash2 &*= 0x100000001b3
        }
        withUnsafeBytes(of: hash2.littleEndian) { buf in
            for i in 0..<8 { bytes[8 + i] = buf[i] }
        }
        bytes[6] = (bytes[6] & 0x0F) | 0x50
        bytes[8] = (bytes[8] & 0x3F) | 0x80
        let uuid = bytes.withUnsafeBytes { $0.load(as: uuid_t.self) }
        return UUID(uuid: uuid)
    }

    static func message(from m: ErisConversationMessage) -> Message {
        Message(
            id: stableUUID(from: m.id),
            sender: m.isUser ? .user : .herald,
            content: m.body,
            status: m.isFailed ? .failed : .sent,
            toolActivities: m.tools.map {
                ToolActivity(
                    label: $0.name,
                    name: $0.name,
                    resultPreview: $0.output,
                    isError: $0.isError
                )
            },
            reasoning: m.displayReasoning ?? ""
        )
    }

    static func summary(from s: ErisConversationSummary) -> SessionSummary {
        SessionSummary(
            id: stableUUID(from: s.id),
            title: s.displayTitle,
            previewText: "",
            lastActivity: ErisTimestamp.date(from: s.updatedAt) ?? .now,
            sessionKey: s.id
        )
    }
}
