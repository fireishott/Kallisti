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
                attachments: uploaded,
                // Stable for the life of this message, across every retry: a resend
                // after a dropped stream must resume the turn it started, not ask
                // the model the same question twice.
                idempotencyKey: clientMessageID.uuidString
            )
            let jobID = Self.stableUUID(from: turn.jobID)
            continuation.yield(.messageSent(jobID: jobID, daemonJobID: turn.jobID))

            var text = ""
            // The turn announces its route once, on the opening frame, so the
            // finished row is what carries it: a delta stream has no other
            // place to keep it.
            var turnRoute: TurnRoute?
            let stream = await client.events(turnID: turn.jobID)
            for try await frame in stream {
                // Only the delta frames need accumulating; the terminal frame
                // carries the assembled answer.
                if case .delta(let delta) = frame { text += delta }
                if case .turnStarted(_, let announced) = frame { turnRoute = announced }
                guard let mapped = Self.streamingUpdate(
                    for: frame,
                    jobID: jobID,
                    messageID: clientMessageID,
                    clientMessageID: clientMessageID,
                    accumulatedText: text,
                    route: turnRoute
                ) else { continue }
                continuation.yield(mapped.update)
                if mapped.isTerminal {
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

    // MARK: - Reattach

    /// Attach to a turn this device already sent, from the tail of its stream.
    ///
    /// The live send's stream dies when iOS reaps the socket; the turn does
    /// not. `lastEventID` is nil on purpose: there is no sequence to replay
    /// from (the send path's decoder kept the last id in memory and that
    /// memory is what the suspension took), so this watches from the tail.
    /// The terminal frame carries the assembled answer, so nothing that
    /// arrived while this device was away is missing from the reply that
    /// settles it - only the intermediate deltas are unseen.
    func resumeStreaming(jobID: String) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            let task = Task { @MainActor [weak self] in
                guard let self else {
                    continuation.finish()
                    return
                }
                await self.performStreamingResume(jobID: jobID, continuation: continuation)
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func performStreamingResume(
        jobID: String,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        guard let client = await pairedClient() else {
            continuation.yield(.failed("No Eris daemon is paired."))
            continuation.finish()
            return
        }
        // The row the finished update carries: this path never owned the
        // client message id, so the job's own stable id is the row identity.
        let turnID = Self.stableUUID(from: jobID)
        var text = ""
        // Attaching from the tail usually misses the opening frame, so a
        // reattached turn has no route to show. That is honest: the badge is
        // absent rather than guessed.
        var turnRoute: TurnRoute?
        do {
            let stream = await client.events(turnID: jobID, lastEventID: nil)
            for try await frame in stream {
                if case .delta(let delta) = frame { text += delta }
                if case .turnStarted(_, let announced) = frame { turnRoute = announced }
                guard let mapped = Self.streamingUpdate(
                    for: frame,
                    jobID: turnID,
                    messageID: turnID,
                    clientMessageID: nil,
                    accumulatedText: text,
                    route: turnRoute
                ) else { continue }
                continuation.yield(mapped.update)
                if mapped.isTerminal {
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

    /// One frame as the chat reads it.
    ///
    /// The live send and a reattach read the same `events(...)` stream, so both
    /// map through this one function: a frame decoded differently on the two
    /// paths would make a reattached turn disagree with the reply it settles
    /// into. `messageID` is the row the finished update carries - the client
    /// message id for a live send (the row it already owns), the job's stable
    /// id for a reattach (which has no user row of its own).
    ///
    /// `nil` means the frame has no chat-facing update (a `tool_call` step,
    /// which duplicates the tool_start frame above), and `isTerminal` means
    /// this frame ends the turn and the stream after it.
    private static func streamingUpdate(
        for frame: ErisFrame,
        jobID: UUID,
        messageID: UUID,
        clientMessageID: UUID?,
        accumulatedText: String,
        route: TurnRoute?
    ) -> (update: StreamingUpdate, isTerminal: Bool)? {
        switch frame {
        case .turnStarted:
            // No chat-facing update of its own: the route rides on the finished
            // row (the caller threads it in), and the model name the frame
            // carries already has the toolbar's own source.
            return nil
        case .delta(let delta):
            return (.textDelta(delta), false)
        case .reasoning(let fragment):
            return (.reasoningDelta(fragment), false)
        case .artifactDiff(let diff):
            // One card per turn, drawn by the same view the relay path used.
            // No second diff UI.
            return (.artifactDiff(diff.codeDiff), false)
        case .artifactPage(let html, let cap):
            return (.artifactPage(html: html, heightCap: cap), false)
        case .attachment(let file):
            return (.attachment(Self.messageAttachment(from: file)), false)
        case .approval(let approval):
            // Visible now; the decide card is its own commit.
            return (.toolActivity(
                "approval needed: " + approval.tool + " " + approval.arguments
            ), false)
        case .ask(let question, let choices, let requestID, let multi):
            // The app already draws a clarify card for this exact shape.
            return (.clarifyRequest(
                question: question,
                choices: choices,
                requestID: requestID,
                multiSelect: multi
            ), false)
        case .toolStart(let name):
            // The daemon sends both a step frame and a tool_start for the same
            // call. The tool_start is the one that carries the id the output
            // and completion frames match on, so it owns the live row.
            return (.toolStarted(ToolActivity(
                label: name,
                toolCallID: name,
                name: name
            )), false)
        case .toolFinished(let name, let output, let isError):
            return (.toolCompleted(
                toolCallID: name,
                resultPreview: String(output.prefix(1200)),
                isError: isError,
                durationMs: nil
            ), false)
        case .toolOutput(let name, let chunk):
            return (.toolOutput(toolCallID: name, chunk: chunk), false)
        case .step(let label, let stepType):
            // A tool_call step duplicates the tool_start above. Other steps are
            // activity lines, not answer text.
            if stepType == "tool_call" { return nil }
            return (.toolActivity(label), false)
        case .round(let count, let total, _):
            // Nothing in the transcript changes: this is the live counter.
            return (.round(count: count, total: total), false)
        case .done(let full, let usage, let accounting):
            let content = full.isEmpty ? accumulatedText : full
            let final = Message(
                id: messageID,
                clientMessageID: clientMessageID,
                sender: .herald,
                content: content,
                jobID: jobID,
                status: .sent,
                // The receipt the footer draws. It arrives on the terminal frame
                // and was dropped here, so the footer under a live turn had no
                // numbers until the chat was reopened and read back from the
                // transcript. That is why the footer looked like it only worked
                // in some sessions: it worked after a reload, and not while the
                // answer was arriving.
                accounting: accounting,
                route: route
            )
            let tokenUsage = usage.map {
                TokenUsage(
                    promptTokens: $0.inputTokens ?? 0,
                    completionTokens: $0.outputTokens ?? 0,
                    totalTokens: ($0.inputTokens ?? 0) + ($0.outputTokens ?? 0)
                )
            }
            return (.finished(final, tokenUsage, nil, nil), true)
        case .error(let error):
            return (.failed(error.errorDescription ?? "turn failed", category: error.code), true)
        case .cancelled:
            return (.cancelled, true)
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
                current.messages = Self.messages(from: detail)
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
    /// The daemon's settings record.
    func settings() async throws -> ErisSettingsSnapshot {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.settings()
    }

    /// Start the update the check offered.
    func updateApply() async throws {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        try await client.updateApply()
    }

    /// How the install is going.
    func updateProgress() async throws -> String {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.updateProgress()
    }

    /// The label and group behind each setting key.
    func settingsLabels() async throws -> [String: String] {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.settingsLabels()
    }

    /// Every setting the daemon holds, flat.
    func settingsFields() async throws -> [String: String] {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.settingsFields()
    }

    /// Write settings back.
    func updateSettings(_ fields: [String: String]) async throws {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        try await client.updateSettings(fields)
    }

    /// The daemon's schedules. Without this the protocol default returns an empty
    /// list and the screen reads "No Cron Jobs" while the daemon has plenty.
    func cronJobs() async throws -> [ErisCronJob] {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.cronJobs()
    }

    func createCronJob(name: String, schedule: String, prompt: String) async throws -> ErisCronJob {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.createCronJob(name: name, schedule: schedule, prompt: prompt)
    }

    func setCronJobEnabled(id: String, enabled: Bool) async throws -> ErisCronJob {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.setCronJobEnabled(id: id, enabled: enabled)
    }

    func deleteCronJob(id: String) async throws {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        try await client.deleteCronJob(id: id)
    }

    /// The daemon's skill catalogue, same rule.
    func skillCatalog() async throws -> [ErisSkill] {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.skillCatalog()
    }

    /// Round trip to the daemon, for the Connection row.
    func pingLatencyMs() async -> Int? {
        guard let client = await pairedClient() else { return nil }
        return await client.pingLatencyMs()
    }

    /// The daemon's own health. Admin scope on the daemon side.
    func gatewayHealth() async throws -> ErisGatewayHealth {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.gatewayHealth()
    }

    /// The diagnostics ring behind the Logs screen.
    func diagnosticsBundle() async throws -> ErisDiagnostics {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.diagnosticsBundle()
    }

    /// Whether the daemon has a newer build. Admin scope on the daemon side.
    func updateCheck() async throws -> ErisUpdateCheck {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        return try await client.updateCheck()
    }

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
            messages: Self.messages(from: detail),
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

    /// The children this daemon ran for a chat, newest first. Used by the
    /// subagent badge and its watchdog; an unreachable daemon reads as none.
    func subagents(for localID: UUID?) async -> [ErisSubagent] {
        guard let client = await pairedClient(),
              let page = try? await client.subagents()
        else { return [] }
        guard let localID,
              let conversationID = await serverConversationID(for: localID)
        else { return page.subagents }
        return page.subagents.filter { $0.parentConversation == conversationID }
    }

    func deleteSession(id: UUID) async throws {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        guard let conversationID = await serverConversationID(for: id) else {
            throw ErisHeraldError.notConnected
        }
        try await client.deleteConversation(id: conversationID)
        conversationIDByLocalID.removeValue(forKey: id)
        localIDByConversationID.removeValue(forKey: conversationID)
    }

    func archiveSession(id: UUID) async throws {
        throw ErisHeraldError.notBackedYet("archiveSession")
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        throw ErisHeraldError.notBackedYet("togglePinSession")
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        guard let conversationID = await serverConversationID(for: id) else {
            throw ErisHeraldError.notConnected
        }
        let renamed = try await client.renameConversation(id: conversationID, title: title)
        conversationIDByLocalID[id] = conversationID
        localIDByConversationID[conversationID] = id
        return SessionSummary(
            id: id,
            title: renamed.title,
            sessionKey: conversationID
        )
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        throw ErisHeraldError.notBackedYet("generateSessionTitle")
    }

    func generateCreativeTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        throw ErisHeraldError.notBackedYet("generateCreativeTitle")
    }

    // MARK: - Jobs

    /// The relay's status call, which the Eris client cannot answer from a hash.
    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
        nil
    }

    /// Ask the daemon about one job, by the id it gave back.
    func jobState(daemonJobID: String) async -> LiveHeraldClient.JobStatusResponse? {
        guard let client = await pairedClient() else { return nil }
        guard let state = try? await client.jobState(daemonJobID) else { return nil }
        // The outbox speaks in the relay's vocabulary: a job that reached any
        // terminal state reads "terminal" whatever word the daemon used.
        let raw = (state.state ?? "").lowercased()
        let terminal = ["succeeded", "failed", "cancelled", "canceled", "interrupted", "expired"]
        return LiveHeraldClient.JobStatusResponse(
            status: terminal.contains(raw) ? "terminal" : raw,
            conversationId: nil,
            message: nil,
            error: nil,
            usage: nil,
            context: nil,
            diff: nil,
            attempt: nil,
            lastSeq: nil,
            errorCategory: nil,
            errorAction: nil
        )
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
        // Same rule as the streaming path: one message, one turn, however many
        // times this call is retried.
        let turn = try await client.createTurn(
            text: text,
            conversation: erisConversationID,
            idempotencyKey: clientMessageID.uuidString
        )
        // Drain the stream to completion to collect the final text.
        var text = ""
        let stream = await client.events(turnID: turn.jobID)
        for try await frame in stream {
            if case .done(let full, _, _) = frame { text = full; break }
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

    // MARK: - Notes

    /// One note sync, without streaming. Used by any caller that wants the final
    /// answer only; the engine itself takes the streaming form.
    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message {
        var final: Message?
        for await update in sendNoteMessageStreaming(
            text: text,
            attachments: attachments,
            clientMessageID: clientMessageID,
            conversationID: conversationID,
            title: title,
            enrichmentModelName: nil,
            enrichmentProvider: nil
        ) {
            if case .finished(let message, _, _, _) = update { final = message }
        }
        return final ?? Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .herald,
            content: "No reply received.",
            status: .failed
        )
    }

    /// A note is a conversation: the note's stable local id binds to one daemon
    /// conversation on first sync, the drawing rides as an attachment, and every
    /// later edit appends a turn to the same thread.
    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String?, enrichmentProvider: String?) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            let task = Task { @MainActor [weak self] in
                await self?.performNoteSyncStreaming(
                    text: text,
                    attachments: attachments,
                    clientMessageID: clientMessageID,
                    conversationID: conversationID,
                    title: title,
                    enrichmentModelName: enrichmentModelName,
                    enrichmentProvider: enrichmentProvider,
                    continuation: continuation
                )
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// The historical form with the reasoning flag. Notes do not render a
    /// separate thinking stream from the transport, so the flag is accepted and
    /// ignored rather than made a second code path.
    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String?, enrichmentProvider: String?, thinkingAsReasoning: Bool) -> AsyncStream<StreamingUpdate> {
        sendNoteMessageStreaming(
            text: text,
            attachments: attachments,
            clientMessageID: clientMessageID,
            conversationID: conversationID,
            title: title,
            enrichmentModelName: enrichmentModelName,
            enrichmentProvider: enrichmentProvider
        )
    }

    private func performNoteSyncStreaming(
        text: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        conversationID: UUID,
        title: String,
        enrichmentModelName: String?,
        enrichmentProvider: String?,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        guard let client = await pairedClient() else {
            continuation.yield(.failed("No Eris daemon is paired."))
            continuation.finish()
            return
        }

        // The note's local id is the key; the daemon id is the row a turn is
        // written into. A note that has never synced gets its conversation now.
        if conversationIDByLocalID[conversationID] == nil,
           !(await ensureConversation(id: conversationID)) {
            continuation.yield(.failed("Could not open a conversation for this note."))
            continuation.finish()
            return
        }
        guard let boundConversation = conversationIDByLocalID[conversationID] else {
            continuation.yield(.failed("Could not open a conversation for this note."))
            continuation.finish()
            return
        }

        var uploaded: [String] = []
        for attachment in attachments {
            do {
                let id = try await client.uploadAttachment(
                    data: attachment.data,
                    fileName: attachment.fileName,
                    mediaType: attachment.mimeType,
                    conversationID: boundConversation
                )
                uploaded.append(id)
            } catch {
                let reason = (error as? ErisError)?.message ?? error.localizedDescription
                continuation.yield(.failed(
                    "Uploading \(attachment.fileName) failed: \(reason)", category: nil
                ))
                continuation.finish()
                return
            }
        }

        // The enrichment model, when one is pinned. A bare id is accepted by the
        // daemon only when exactly one catalogue carries it, so a provider is
        // named whenever the picker recorded one.
        var model: String?
        if let name = enrichmentModelName?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
            if let provider = enrichmentProvider?.trimmingCharacters(in: .whitespacesAndNewlines),
               !provider.isEmpty {
                // Always name the provider: a model id like
                // `deepseek/deepseek-v4-flash` has a head that is also a
                // provider id, and the daemon would read it as that provider.
                model = "\(provider)/\(name)"
            } else {
                model = name
            }
        }

        do {
            let turn = try await client.createTurn(
                text: text,
                conversation: boundConversation,
                attachments: uploaded,
                idempotencyKey: clientMessageID.uuidString,
                model: model,
                title: title
            )
            let jobID = Self.stableUUID(from: turn.jobID)
            continuation.yield(.messageSent(jobID: jobID, daemonJobID: turn.jobID))

            var accumulated = ""
            let stream = await client.events(turnID: turn.jobID)
            for try await frame in stream {
                switch frame {
                case .delta(let delta):
                    accumulated += delta
                    continuation.yield(.textDelta(delta))
                case .reasoning(let fragment):
                    continuation.yield(.reasoningDelta(fragment))
                case .step(let label, _):
                    continuation.yield(.toolActivity(label))
                case .done(let full, let usage, let accounting):
                    let content = full.isEmpty ? accumulated : full
                    let final = Message(
                        id: clientMessageID,
                        clientMessageID: clientMessageID,
                        sender: .herald,
                        content: content,
                        jobID: jobID,
                        status: .sent,
                        // The same receipt the streaming path carries, so a
                        // note answer and a chat answer draw the same footer.
                        accounting: accounting
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
                    continuation.yield(.failed(
                        error.errorDescription ?? "turn failed", category: error.code
                    ))
                    continuation.finish()
                    return
                case .cancelled:
                    continuation.yield(.cancelled)
                    continuation.finish()
                    return
                default:
                    break
                }
            }
            continuation.finish()
        } catch {
            continuation.yield(.failed(
                (error as? ErisError)?.errorDescription ?? error.localizedDescription
            ))
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

    /// The daemon's id for a chat the rail shows, even when this process has
    /// never opened it. A session row's local id is derived from the daemon id,
    /// so the list can answer without a binding; this binds it once found.
    private func serverConversationID(for localID: UUID) async -> String? {
        if let bound = conversationIDByLocalID[localID] { return bound }
        guard let client = await pairedClient(),
              let page = try? await client.conversations(),
              let match = page.conversations.first(where: {
                  Self.stableUUID(from: $0.id) == localID
              })
        else { return nil }
        conversationIDByLocalID[localID] = match.id
        localIDByConversationID[match.id] = localID
        return match.id
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

    /// One attachment as the app draws it. A scheme of our own, so the view
    /// fetches the bytes from the paired daemon instead of a relay that never
    /// had them; the id is the daemon's row, not a local UUID.
    static func messageAttachment(from file: ErisAttachmentRef) -> MessageAttachment {
        MessageAttachment(
            id: Self.stableUUID(from: file.id),
            kind: (file.mediaType ?? "").hasPrefix("image/") ? "image" : "file",
            fileName: file.name,
            mimeType: file.mediaType ?? "application/octet-stream",
            thumbnailBase64: nil,
            localStoragePath: nil,
            mediaURL: URL(string: "eris-attachment://" + file.id)
        )
    }

    /// The daemon's own continuation prompt. It is a machine instruction, not
    /// something the operator typed, so the transcript shows a quiet system
    /// line instead of a wall of text in a user bubble.
    static let autoContinuationPrefix =
        "Continue this job. The previous turn ended at its tool-round cap"

    static func message(from m: ErisConversationMessage) -> Message {
        let isAutoContinuation = m.isUser
            && m.body.hasPrefix(autoContinuationPrefix)
        return Message(
            id: stableUUID(from: m.id),
            // The two handles the daemon stamps on the row. Without them a
            // refreshed transcript could only be matched to the bubble on screen
            // by its text, and a streamed answer that differed by a trailing
            // newline came back as a second copy of itself: one row in the
            // database, two bubbles on the phone.
            clientMessageID: m.clientMessageID.flatMap { UUID(uuidString: $0) },
            sender: isAutoContinuation ? .system : (m.isUser ? .user : .herald),
            content: isAutoContinuation
                ? "Continued automatically after the tool-round limit."
                : m.body,
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
            attachments: m.attachments.map(Self.messageAttachment(from:)),
            reasoning: m.displayReasoning ?? "",
            erisID: m.id,
            accounting: m.accounting
        )
    }

    /// The receipt for one row, with the conversation's own totals folded in.
    ///
    /// Two of the footer's fields are conversation-scoped and cannot be read off
    /// a row: the turn counter would restart on every older page, and a session
    /// total summed from the rows on screen would be a floor presented as a
    /// total. The daemon totals them over the conversation it just sent, so they
    /// are folded in here and every answer shows the same session figures.
    ///
    /// Only the newest answer keeps the conversation-level cost, because a
    /// session total on each of forty bubbles reads as forty separate charges.
    /// The turn counter is the exception: it is the same number for every row and
    /// reads correctly on all of them.
    static func receipt(
        for row: ErisConversationMessage,
        totals: ErisConversationTotals?,
        isNewestAnswer: Bool
    ) -> TurnAccounting? {
        guard let totals else { return row.accounting }
        let base = row.accounting
        // Everything that comes from the row, resolved before the initialiser.
        //
        // The single expression below grew past what Swift's type checker will
        // infer in reasonable time once the nil-coalescing chains were added, and a
        // failed build is a feature that does not ship. Naming the row-derived
        // values costs nothing and gives the checker less to infer at once.
        let rowTools = row.tools
        let rowToolCount: Int? = rowTools.isEmpty ? nil : rowTools.count
        let rowSkillCount: Int? = rowTools.isEmpty
            ? nil
            : rowTools.filter { $0.name == "read_skill" }.count
        let resolvedContext = base?.contextTokens ?? (isNewestAnswer ? totals.contextUsed : nil)
        let resolvedWindow = base?.contextWindow ?? (isNewestAnswer ? totals.contextWindow : nil)
        let resolvedCost = base?.costUSD ?? (isNewestAnswer ? totals.turnCostUSD : nil)
        let resolvedSessionCost: Double? = isNewestAnswer
            ? (base?.sessionCostUSD ?? totals.sessionCostUSD)
            : nil
        // The row's own measured numbers win; the totals fill in only what a row
        // cannot know. A session that lost its per-row receipt still gets its
        // turn counter, and a row that measured nothing still gets a footer when
        // the conversation has a cost to report.
        return TurnAccounting(
            model: base?.model,
            provider: base?.provider,
            promptTokens: base?.promptTokens,
            completionTokens: base?.completionTokens,
            cachedTokens: base?.cachedTokens,
            durationMs: base?.durationMs,
            rounds: base?.rounds,
            contextTokens: resolvedContext,
            contextWindow: resolvedWindow,
            // The row's own cost when it has one: that is the figure that
            // belongs under this answer.
            costUSD: resolvedCost,
            sessionCostUSD: resolvedSessionCost,
            pricedAnswers: totals.pricedAnswers,
            sessionAnswers: totals.answers,
            sessionTurns: totals.turns,
            toolCalls: base?.toolCalls ?? rowToolCount,
            // The learned line is a conversation fact, so it comes from the
            // totals and not from the row. It was added to `ErisConversationTotals`
            // and never copied onto the receipt, so the sub-footer could not draw
            // no matter what the daemon sent.
            skillsCreated: totals.skillsCreated,
            skillsAppended: totals.skillsAppended,
            skillCalls: base?.skillCalls ?? rowSkillCount,
            capped: base?.capped,
            wrappedUp: base?.wrappedUp
        )
    }

    /// Every row of a fetched conversation, with the session figures folded in.
    static func messages(
        from detail: ErisConversationDetail
    ) -> [Message] {
        // The newest settled answer is the one that carries the conversation's
        // cost and context figures. Indexing first keeps the marking honest when
        // the last row is a canned system notice rather than an answer.
        let newestAnswerID = detail.messages
            .last(where: { !$0.isUser && !$0.isFailed })?
            .id
        return detail.messages.map { row in
            var message = Self.message(from: row)
            if !row.isUser {
                message.accounting = Self.receipt(
                    for: row,
                    totals: detail.totals,
                    isNewestAnswer: row.id == newestAnswerID
                )
            }
            return message
        }
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
