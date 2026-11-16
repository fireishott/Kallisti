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

    /// A turn comes back as frames; there is no other form to ask for.
    var streamsOnly: Bool { true }
    private(set) var currentConversation: Conversation? {
        didSet { persistOpenConversation() }
    }

    /// Owns pairing and the sensor pipeline; the transport is pulled from it.
    private let store = ErisDaemonStore.shared

    /// Local conversation UUID <-> Eris conversation id (opaque string).
    ///
    /// Persisted. This map is the only thing that knows which daemon
    /// conversation a chat on this phone refers to, so keeping it in memory meant
    /// a relaunch could not ask for its own history: the transcript came back as
    /// an empty New Chat while the daemon still held every message. It also meant
    /// a first send could go out with no conversation at all, which is why the
    /// first reply vanished.
    private var conversationIDByLocalID: [UUID: String] = [:] {
        didSet { persistConversationMap() }
    }
    private var localIDByConversationID: [String: UUID] = [:]

    /// Attachment ids already uploaded this session, keyed by the pending file.
    /// The early upload and the send path share this, so a file moves once.
    private var uploadedAttachmentIDs: [UUID: String] = [:]

    private static let conversationMapKey = "eris.conversationMap.v1"

    /// Write the map through. Called from the `didSet` above, so every insertion
    /// persists without the callers having to remember.
    private func persistConversationMap() {
        let encoded = conversationIDByLocalID.reduce(into: [String: String]()) { out, pair in
            out[pair.key.uuidString] = pair.value
        }
        UserDefaults.standard.set(encoded, forKey: Self.conversationMapKey)
    }

    /// The daemon conversation bound to a local chat, if one is bound.
    ///
    /// Internal rather than private so the persistence can be tested: this map is
    /// the layer that decides whether a phone finds its own history, and an
    /// untested map is how it stayed in memory for three builds.
    func boundConversationID(for localID: UUID) -> String? {
        conversationIDByLocalID[localID]
    }

    private static let openConversationKey = "eris.openConversation.v1"

    /// Remember which chat is open. Without this a relaunch opens a new one, and
    /// the transcript of the old one is unreachable even though the daemon has it.
    private func persistOpenConversation() {
        guard let open = currentConversation else { return }
        UserDefaults.standard.set(
            [open.id.uuidString, open.title],
            forKey: Self.openConversationKey
        )
    }

    /// The chat that was open before, if there was one.
    private func restoreOpenConversation() {
        guard let stored = UserDefaults.standard.array(forKey: Self.openConversationKey) as? [String],
              stored.count == 2,
              let id = UUID(uuidString: stored[0])
        else { return }
        currentConversation = Conversation(id: id, title: stored[1])
    }

    /// Read the map back, rebuilding both directions.
    private func loadConversationMap() {
        guard let encoded = UserDefaults.standard.dictionary(forKey: Self.conversationMapKey)
                as? [String: String] else { return }
        for (local, conversation) in encoded {
            guard let uuid = UUID(uuidString: local) else { continue }
            conversationIDByLocalID[uuid] = conversation
            localIDByConversationID[conversation] = uuid
        }
    }

    // MARK: - Connection

    init() {
        loadConversationMap()
        restoreOpenConversation()
    }

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
                    attachments: attachments,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// What a send needs before it can be stored.
    ///
    /// Split out of the send path so it can be tested without a transport. Three
    /// defects in this area were only visible when the code ran, so the decision -
    /// bind, reuse, or refuse - is a function with its inputs passed in.
    enum SendBinding: Equatable {
        /// The daemon conversation this send will be written to.
        case ready(String)
        /// No conversation could be bound, so there is nowhere to write it.
        case refused
    }

    /// Decide the conversation a send belongs to, binding one if needed.
    ///
    /// - Parameters:
    ///   - localID: the local chat, or nil when the client has none.
    ///   - lookup: the already-bound daemon conversation for a local chat.
    ///   - ensure: bind one, answering whether it succeeded.
    static func bindForSend(
        localID: UUID?,
        lookup: (UUID) -> String?,
        ensure: (UUID) async -> Bool
    ) async -> SendBinding {
        guard let localID else { return .refused }
        if let bound = lookup(localID) { return .ready(bound) }
        // Two attempts: the first can lose a race with the pairing handshake on a
        // cold launch, and the second costs nothing when the first worked.
        for _ in 0..<2 {
            _ = await ensure(localID)
            if let bound = lookup(localID) { return .ready(bound) }
        }
        return .refused
    }

    private func performStreamingSend(
        message: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        guard let client = await pairedClient() else {
            continuation.yield(.failed("No Eris daemon is paired."))
            continuation.finish()
            return
        }

        do {
            // Eris turns carry text only; attachments have no route yet and are
            // dropped (the composer's text still reaches the model).
            //
            // Bind a conversation first. This is the path the chat actually takes,
            // and a turn sent with no conversation is a turn with no transcript
            // row: the answer streams once and then exists nowhere, which is the
            // disappearing reply and the empty history in one.
            // A chat with no current conversation cannot be bound at all, so give
            // it one: an unbound chat is the bug this whole path is fixing.
            if currentConversation == nil {
                currentConversation = Conversation(title: "New Chat")
            }
            let binding = await Self.bindForSend(
                localID: currentConversation?.id,
                lookup: { conversationIDByLocalID[$0] },
                ensure: { await ensureConversation(id: $0) }
            )
            guard case .ready(let boundConversation) = binding else {
                // Sending anyway is the original bug: a turn with no conversation
                // produces an answer that exists nowhere, with nothing on screen to
                // explain it.
                continuation.yield(.failed(
                    "Could not open a conversation with the daemon, so the message was not sent."
                ))
                continuation.finish()
                return
            }
            // The files go first: a turn can only name ids that exist, and the
            // operator is told which one is in flight and whether it landed.
            var uploaded: [String] = []
            for (index, attachment) in attachments.enumerated() {
                continuation.yield(.uploading(
                    attachmentID: attachment.id,
                    completed: index,
                    total: attachments.count,
                    name: attachment.fileName
                ))
                do {
                    let id: String
                    if let already = uploadedAttachmentIDs[attachment.id] {
                        id = already
                    } else {
                        id = try await client.uploadAttachment(
                            data: attachment.data,
                            fileName: attachment.fileName,
                            mediaType: attachment.mimeType,
                            conversationID: boundConversation
                        )
                        uploadedAttachmentIDs[attachment.id] = id
                    }
                    uploaded.append(id)
                } catch {
                    // Sending a turn without the file the operator attached would
                    // be the same lie as dropping it: the message would exist and
                    // the picture would not.
                    let reason = (error as? ErisError)?.message ?? error.localizedDescription
                    continuation.yield(.failed(
                        "Uploading \(attachment.fileName) failed: \(reason)", category: nil
                    ))
                    continuation.finish()
                    return
                }
            }
            if !attachments.isEmpty {
                // Every file landed: the chips can go.
                for attachment in attachments {
                    continuation.yield(.uploading(
                        attachmentID: attachment.id,
                        completed: attachments.count,
                        total: attachments.count,
                        name: attachment.fileName
                    ))
                }
            }
            let turn = try await client.createTurn(
                text: message,
                conversation: boundConversation,
                attachments: uploaded
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
                case .reasoning(let fragment):
                    continuation.yield(.reasoningDelta(fragment))
                case .artifactDiff(let diff):
                    // One card per turn, drawn by the same view the relay path
                    // used. No second diff UI.
                    continuation.yield(.artifactDiff(diff.codeDiff))
                case .artifactPage(let html, let cap):
                    continuation.yield(.artifactPage(html: html, heightCap: cap))
                case .approval(let approval):
                    // Visible now; the decide card is its own commit.
                    continuation.yield(.toolActivity(
                        "approval needed: " + approval.tool + " " + approval.arguments
                    ))
                case .ask(let question, let choices, let requestID, let multi):
                    // The app already draws a clarify card for this exact shape.
                    continuation.yield(.clarifyRequest(
                        question: question,
                        choices: choices,
                        requestID: requestID,
                        multiSelect: multi
                    ))
                case .step(let label, _):
                    // A step is an activity line, not answer text: it lands in the
                    // Thought process row the UI already draws.
                    continuation.yield(.toolActivity(label))
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
        if var current = currentConversation {
            // A restored chat is a shell: an id and a title, with no messages. It
            // must be filled before it is answered, or the transcript stays blank
            // while the daemon holds every row and pushes announce the replies.
            guard current.messages.isEmpty else { return current }
            if let boundID = conversationIDByLocalID[current.id],
               let client = await pairedClient(),
               let detail = try? await client.conversation(id: boundID) {
                current.messages = detail.messages.map { Self.message(from: $0) }
                currentConversation = current
            }
            return current
        }
        // Nothing restored: hand back an empty one the chat can use. This is a
        // first launch, not a lost transcript - a restored chat returns above.
        let conversation = Conversation(title: "New Chat")
        currentConversation = conversation
        return conversation
    }

    /// The page of transcript before `before`, for a transcript scrolled to the top.
    ///
    /// Returns the older messages oldest-first and whether another page exists. An
    /// unknown conversation or a daemon without the cursor answers nothing rather
    /// than throwing into the view: a transcript that cannot page is a transcript
    /// that shows what it has.
    /// Upload one picked file now, so its chip can show a spinner and a check.
    func preuploadAttachment(_ attachment: PendingAttachment, conversationID: UUID?) async -> String? {
        if let done = uploadedAttachmentIDs[attachment.id] { return done }
        guard let client = await pairedClient() else { return nil }
        var bound = conversationID.flatMap { conversationIDByLocalID[$0] }
        if bound == nil, let local = currentConversation?.id {
            _ = await ensureConversation(id: local)
            bound = conversationIDByLocalID[local]
        }
        guard let id = try? await client.uploadAttachment(
            data: attachment.data,
            fileName: attachment.fileName,
            mediaType: attachment.mimeType,
            conversationID: bound
        ) else { return nil }
        uploadedAttachmentIDs[attachment.id] = id
        return id
    }

    func loadOlder(conversationID: UUID, before: String) async -> (messages: [Message], hasMore: Bool) {
        guard let client = await pairedClient(),
              let erisID = conversationIDByLocalID[conversationID],
              let detail = try? await client.conversation(id: erisID, before: before)
        else {
            return ([], false)
        }
        return (detail.messages.map { Self.message(from: $0) }, detail.hasMore ?? false)
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        var conversationID = conversationIDByLocalID[id]
        if conversationID == nil {
            // The id may be a stable id derived from a conversation the map has
            // never seen - every chat created before the id scheme was unified.
            // Ask the daemon which one it is, then bind it.
            if let page = try? await client.conversations() {
                conversationID = page.conversations
                    .first(where: { Self.stableUUID(from: $0.id) == id })?
                    .id
            }
        }
        guard let conversationID else {
            return Conversation(id: id, title: "New Chat")
        }
        // Bind it. A chat opened from the session list - one started on another
        // device - resolved to a conversation here; leaving the map empty meant the
        // next send created a fresh conversation and the reply diverged into a new
        // chat instead of continuing this one.
        conversationIDByLocalID[id] = conversationID
        localIDByConversationID[conversationID] = id
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
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        if let conversationID = currentConversationID() {
            _ = try await client.clearConversation(id: conversationID)
        }
        let conversation = Conversation(title: "New Chat")
        currentConversation = conversation
        return conversation
    }

    func ensureConversation(id: UUID) async -> Bool {
        if conversationIDByLocalID[id] != nil { return true }
        guard let client = await pairedClient() else { return false }
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
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
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
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
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
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        // A turn with no conversation is a turn with no transcript row: the reply
        // streams once and then exists nowhere. Bind first, and refuse to send
        // rather than send into the void.
        if conversationIDByLocalID[conversationID] == nil {
            guard await ensureConversation(id: conversationID) else {
                throw ErisHeraldError.notConnected
            }
        }
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

    /// The transport for the paired daemon, re-discovering the pair if it was
    /// redeemed after `connect()` already ran. Exposed so the Eris inbox/notes
    /// services can reach the same transport the chat stores use.
    func client() async -> ErisClient? {
        await pairedClient()
    }

    /// A transport for the paired daemon, re-discovering the pair if it was
    /// redeemed by `ErisPairingService` after `connect()` already ran. `store
    /// .start()` is idempotent, so a device that just paired lands straight in.
    private func pairedClient() async -> ErisClient? {
        if let client = store.clientForPairedDaemon() {
            return client
        }
        await store.start()
        if store.pairedBaseURL != nil {
            connectionStatus = .connected
        }
        return store.clientForPairedDaemon()
    }

    private func currentConversationID() -> String? {
        guard let currentConversation else { return nil }
        return conversationIDByLocalID[currentConversation.id]
    }

    private func localID(forConversation conversationID: String) -> UUID {
        if let existing = localIDByConversationID[conversationID] { return existing }
        // The same id the session list derives, so a row in the drawer and a chat
        // in the map are the same chat. A random UUID here meant opening an old
        // chat asked the map for an id it had never stored.
        let local = Self.stableUUID(from: conversationID)
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
            // The two handles the daemon stamps on the row. Without them a
            // refreshed transcript could only be matched to the bubble on screen
            // by its text, and a streamed answer that differed by a trailing
            // newline came back as a second copy of itself: one row in the
            // database, two bubbles on the phone.
            clientMessageID: m.clientMessageID.flatMap { UUID(uuidString: $0) },
            sender: m.isUser ? .user : .herald,
            content: m.body,
            timestamp: ErisTimestamp.date(from: m.createdAt) ?? .now,
            jobID: m.jobID.flatMap { $0.isEmpty ? nil : Self.stableUUID(from: $0) },
            status: m.isFailed ? .failed : .sent,
            toolActivities: m.tools.map {
                ToolActivity(
                    label: $0.name,
                    name: $0.name,
                    resultPreview: $0.output,
                    isError: $0.isError
                )
            },
            attachments: m.attachments.map { file in
                MessageAttachment(
                    id: Self.stableUUID(from: file.id),
                    kind: (file.mediaType ?? "").hasPrefix("image/") ? "image" : "file",
                    fileName: file.name,
                    mimeType: file.mediaType ?? "application/octet-stream",
                    thumbnailBase64: nil,
                    localStoragePath: nil,
                    // A scheme of our own, so the view fetches the bytes from the
                    // paired daemon instead of a relay that never had them.
                    mediaURL: URL(string: "eris-attachment://" + file.id)
                )
            },
            reasoning: m.displayReasoning ?? "",
            erisID: m.id
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
