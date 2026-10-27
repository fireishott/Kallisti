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
    /// The local id names no conversation the daemon holds any more.
    ///
    /// A load has to say this out loud. The earlier code answered a miss with an
    /// empty "New Chat", which is indistinguishable from a conversation whose
    /// messages were dropped - the operator saw their chat come back blank.
    case unknownSession

    var errorDescription: String? {
        switch self {
        case .notBackedYet(let feature):
            return "\(feature) is not backed by Eris yet."
        case .notConnected:
            return "No Eris daemon is paired."
        case .unknownSession:
            return "That conversation is no longer on the daemon."
        }
    }
}

@MainActor
final class ErisHeraldClient: HeraldClientProtocol {
    var connectionStatus: ConnectionStatus = .disconnected
    private(set) var currentConversation: Conversation?

    /// Owns pairing and the sensor pipeline; the transport is pulled from it.
    ///
    /// Injectable so a test can drive the real client against a stubbed
    /// transport and exercise the exact path the app takes (list the sessions,
    /// open one, send into it) instead of a copy of that logic.
    private let store: ErisDaemonStore

    /// Local conversation UUID <-> Eris conversation id (opaque string).
    ///
    /// One conversation has exactly ONE local id, and it is derived from the
    /// daemon's own id (`localID(forConversation:)`), never minted fresh. The
    /// three paths that name a conversation - the session list, the transcript
    /// loader, and the turn that files a message - all resolve through this map,
    /// so an id the sidebar renders is the id the chat can load and send into.
    ///
    /// Bindings are written to `defaults` on the way in and restored on launch,
    /// and `resolveConversationID(for:client:)` heals anything still missing
    /// from the conversation list. A relaunch therefore neither forgets the chat
    /// that was open nor needs the sidebar to have loaded first.
    private var conversationIDByLocalID: [UUID: String] = [:]
    private var localIDByConversationID: [String: UUID] = [:]

    /// Local note UUID -> the daemon's note id. A note is not a conversation on
    /// Eris: it is a note with runs against it, so the mapping is resolved from
    /// `GET /v1/notes` and cached rather than minted locally.
    private var erisNoteIDByLocalID: [UUID: String] = [:]

    /// The turn this client is currently streaming, and the last one it streamed.
    ///
    /// A clarify answer has to name the turn it is settling, because that is the
    /// resource the daemon resumes. Kept after the stream ends so an answer that
    /// arrives just after a reconnect still names the right job rather than
    /// throwing away the question.
    private var activeTurnID: String?
    private var lastTurnID: String?

    /// Local note UUID -> the newest run opened for it. This is what
    /// `nativeSessionKey` reports, so the note records the key of the run that
    /// actually enriched it rather than a conversation that never existed.
    private var lastRunIDByLocalID: [UUID: String] = [:]

    /// How often the run's event log is polled while an enrichment is running.
    private static let noteRunPollInterval = Duration.milliseconds(1200)

    /// How long one enrichment may run before the client gives up on it. The
    /// daemon keeps working; this is the client's own patience, matching the
    /// 180s the sync engine's watchdog allows.
    private static let noteRunTimeout = Duration.seconds(170)

    /// Where the local UUID <-> conversation bindings outlive a launch.
    private let defaults: UserDefaults
    private static let bindingsKey = "eris.conversation.bindings"

    /// The tool call the turn is running right now, as the row it opened.
    ///
    /// The daemon runs one tool at a time and its frames carry no call id, so the
    /// client mints one per start. That is what lets the stream's start, its live
    /// output and its receipt land on ONE row instead of three, and it is why a
    /// command's stdout can be appended without guessing which call it belongs to.
    private var activeToolRunID: String?
    /// Counts tool runs within this client, so two calls to the same tool are two
    /// rows rather than one that keeps reopening.
    private var toolRunSequence = 0

    init(store: ErisDaemonStore = ErisDaemonStore(), defaults: UserDefaults = .standard) {
        self.store = store
        self.defaults = defaults
        restoreBindings()
    }

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
        guard let client = await pairedClient() else {
            continuation.yield(.failed("No Eris daemon is paired."))
            continuation.finish()
            return
        }

        do {
            // A turn that names no conversation is a turn the transcript can
            // never show: the daemon keeps it as a session of its own, so the
            // answer streams back once and is then unreachable from the session
            // list. Bind the open chat first, and only send once it has a
            // conversation to be filed under.
            if let local = currentConversation?.id, conversationIDByLocalID[local] == nil {
                guard await ensureConversation(id: local) else {
                    continuation.yield(.failed("This chat could not be bound to the daemon. Try again once the host answers."))
                    continuation.finish()
                    return
                }
            }
            // Eris turns carry text only; attachments have no route yet and are
            // dropped (the composer's text still reaches the model).
            let turn = try await client.createTurn(
                text: message,
                conversation: currentConversationID(),
                clientMessageID: clientMessageID
            )
            let jobID = Self.stableUUID(from: turn.jobID)
            self.activeTurnID = turn.jobID
            self.lastTurnID = turn.jobID
            continuation.yield(.messageSent(jobID: jobID))

            var text = ""
            self.activeToolRunID = nil
            let stream = await client.events(turnID: turn.jobID)
            for try await outcome in stream {
                // Reasoning and tool frames are carried, not dropped. The two
                // `activity`/`reasoningDelta` cases below are what fill the
                // thought card and the run log while the turn is still running,
                // instead of after it, from the transcript refetch.
                guard case .existing(let frame) = outcome else {
                    switch outcome {
                    case .reasoning(let reasoning):
                        continuation.yield(.reasoningDelta(reasoning.text))
                    case .round(let round):
                        // Which round is running, before the model says anything.
                        // Without it "thinking" covers a model call, a bash
                        // command and a dead turn alike.
                        continuation.yield(.started(phase: round.phaseLabel))
                    case .toolStart(let start):
                        // A row appears while the command runs, not after it.
                        // The daemon runs one tool at a time, so the run's own
                        // id is enough to tie the start, its output and its
                        // receipt together.
                        self.openToolRun(named: start.name)
                        continuation.yield(
                            .toolStarted(
                                ToolActivity(
                                    label: start.name,
                                    isActive: true,
                                    toolCallID: self.activeToolRunID,
                                    name: start.name,
                                    argsPreview: start.arguments.isEmpty ? nil : start.arguments
                                )
                            )
                        )
                    case .toolOutput(let output):
                        // What the command printed, as it printed it. This is the
                        // stdout the phone had no view of at all: the daemon sent
                        // it, the bridge dropped it as an unknown event name.
                        if let runID = self.activeToolRunID {
                            continuation.yield(
                                .toolOutput(toolCallID: runID, chunk: output.chunk)
                            )
                        }
                    case .tool(let tool):
                        // The receipt, with the whole output attached. It closes
                        // the row the start opened rather than drawing a second
                        // one for the same call.
                        if let runID = self.activeToolRunID {
                            continuation.yield(
                                .toolCompleted(
                                    toolCallID: runID,
                                    resultPreview: tool.output.isEmpty ? nil : tool.output,
                                    isError: tool.isError,
                                    durationMs: nil
                                )
                            )
                            self.activeToolRunID = nil
                        } else {
                            // A daemon that sent no start frame: draw the finished
                            // call as one row, exactly as before.
                            continuation.yield(.toolStarted(tool.activity))
                        }
                    case .asked(let asked):
                        // The turn is parked on a person, not on a model. The card
                        // is what makes it answerable; without this the stream sat
                        // on a spinner until the daemon's own hour ran out.
                        continuation.yield(
                            .clarifyRequest(
                                question: asked.headline,
                                choices: asked.labels.isEmpty ? nil : asked.labels,
                                requestID: asked.id,
                                multiSelect: asked.multiSelect
                            )
                        )
                    case .existing, .ignored:
                        break
                    }
                    continue
                }
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
                    self.activeTurnID = nil
                    self.activeToolRunID = nil
                    continuation.yield(.finished(final, tokenUsage, nil, nil))
                    continuation.finish()
                    return
                case .error(let error):
                    self.activeTurnID = nil
                    self.activeToolRunID = nil
                    continuation.yield(.failed(error.errorDescription ?? "turn failed", category: error.code))
                    continuation.finish()
                    return
                case .cancelled:
                    self.activeTurnID = nil
                    self.activeToolRunID = nil
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

    // MARK: - Clarify

    /// Answer the question the running turn is parked on.
    ///
    /// `requestID` is the gate id the card carries, and the daemon matches it
    /// against what it is waiting on rather than trusting the answer's content.
    /// The turn id comes from this client's own record, because the phone is the
    /// device that opened the turn and is the only party allowed to settle it.
    func respondToClarify(requestID: String, answer: String) async throws {
        _ = requestID
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        guard let turnID = activeTurnID ?? lastTurnID else {
            throw ErisHeraldError.notBackedYet("no turn to answer")
        }
        try await client.answer(turnID: turnID, labels: [], note: nil, text: answer, question: nil)
    }

    /// Read back the question the turn is parked on, for a phone that missed the
    /// live frame: a dropped stream, or a job opened after the card went out.
    func fetchPendingClarify() async -> PendingClarify? {
        guard let client = await pairedClient(),
              let turnID = activeTurnID ?? lastTurnID
        else { return nil }
        guard let asked = try? await client.asked(turnID: turnID) else { return nil }
        guard !asked.headline.isEmpty else { return nil }
        return PendingClarify(
            question: asked.headline,
            choices: asked.labels.isEmpty ? nil : asked.labels,
            requestID: asked.id,
            multiSelect: asked.multiSelect
        )
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
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        // Resolve, do not guess. A session the operator tapped has to end up at
        // the daemon's own conversation, or the transcript arrives empty and the
        // app reports a chat that vanished. A missing binding heals through the
        // list; an id the daemon really does not hold is an error, not a blank
        // "New Chat" that looks like a successful load.
        let conversationID = try await resolveConversationID(for: id, client: client)
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
        // A session that already exists on the daemon only needs its binding
        // restored. Minting a conversation here instead is what left a second
        // empty "New chat" behind on every new chat, and put the turn in a
        // conversation the row the operator was looking at did not name.
        if let existing = try? await resolveConversationID(for: id, client: client) {
            bind(localID: id, to: existing)
            return true
        }
        do {
            let created = try await client.createConversation()
            bind(localID: id, to: created.id)
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
        // Every row is registered on the way out, so the id the sidebar renders
        // is already the id the loader and the send path resolve.
        let sessions = page.conversations.map { summary(from: $0) }
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
        try await createSession(title: title, conversationID: nil)
    }

    /// Start a conversation for an id the UI has already opened.
    ///
    /// The protocol default delegates to the single-argument overload, which
    /// mints its own local UUID from the daemon's new id - so the sidebar row
    /// and the chat the operator is looking at ended up naming two different
    /// ids for one conversation, and neither could load the other's history
    /// (nor send into it: the turn resolved to nothing and was filed as a
    /// conversation of its own). Binding the caller's id is the whole fix.
    func createSession(title: String, conversationID: UUID?) async throws -> SessionSummary {
        guard let client = await pairedClient() else { throw ErisHeraldError.notConnected }
        if let conversationID, let bound = conversationIDByLocalID[conversationID] {
            // Already bound - a retry, not a second conversation.
            let detail = try await client.conversation(id: bound)
            return SessionSummary(
                id: conversationID,
                title: detail.displayTitle,
                sessionKey: bound
            )
        }
        let created = try await client.createConversation()
        let local = conversationID ?? localID(forConversation: created.id)
        bind(localID: local, to: created.id)
        store.noteSession(created.id)
        return SessionSummary(
            id: local,
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
        // Same rule as the streaming path: bind before sending, so the turn is
        // filed in the conversation this id names instead of becoming a session
        // of its own that the transcript can never show.
        if conversationIDByLocalID[conversationID] == nil {
            guard await ensureConversation(id: conversationID) else {
                throw ErisHeraldError.notConnected
            }
        }
        let erisConversationID = conversationIDByLocalID[conversationID]
        let turn = try await client.createTurn(
            text: text,
            conversation: erisConversationID,
            clientMessageID: clientMessageID
        )
        // Drain the stream to completion to collect the final text.
        var text = ""
        let stream = await client.events(turnID: turn.jobID)
        for try await outcome in stream {
            // This path only wants the answer text, so it looks through the two
            // frames it does not render. They still have to be consumed rather
            // than thrown, or a turn that reasons before it answers ends here.
            guard case .existing(let frame) = outcome else { continue }
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

    /// One enrichment, run to completion and returned as a message.
    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message {
        var final: Message?
        var failure: String?
        for await update in sendNoteMessageStreaming(
            text: text,
            attachments: attachments,
            clientMessageID: clientMessageID,
            conversationID: conversationID,
            title: title,
            enrichmentModelName: nil,
            enrichmentProvider: nil,
            thinkingAsReasoning: true
        ) {
            switch update {
            case .finished(let message, _, _, _): final = message
            case .failed(let reason, _, _): failure = reason
            default: break
            }
        }
        if let final { return final }
        return Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .herald,
            content: failure ?? "The enrichment did not finish.",
            status: .failed
        )
    }

    /// Open an enrichment run on the daemon and stream it.
    ///
    /// The stages, in order: resolve the daemon's note id, stage the note's text
    /// as a recognition so the worker has something to read, open a run, then poll
    /// the run's event log. `delta` events become `.textDelta`, and the single
    /// terminal event becomes `.finished` (carrying the markdown the daemon
    /// stored), `.failed` or `.cancelled`.
    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String?, enrichmentProvider: String?, thinkingAsReasoning: Bool) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            let task = Task { @MainActor [weak self] in
                await self?.performNoteEnrichment(
                    text: text,
                    clientMessageID: clientMessageID,
                    conversationID: conversationID,
                    title: title,
                    continuation: continuation
                )
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func performNoteEnrichment(
        text: String,
        clientMessageID: UUID,
        conversationID: UUID,
        title: String,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        guard let client = await pairedClient() else {
            continuation.yield(.failed("No Eris daemon is paired."))
            continuation.finish()
            return
        }
        do {
            let noteID = try await resolveNoteID(conversationID, client: client)

            // The content: the note's own text, staged so the daemon's worker
            // reads what this note says rather than an empty note. A failure here
            // is not fatal on its own - a run against the text the daemon already
            // holds is better than no run - so it is reported and the run goes on.
            let staged = Self.contentForRecognition(text)
            if !staged.isEmpty {
                do {
                    _ = try await client.createNoteRecognition(
                        noteID: noteID,
                        payload: ErisNoteRecognitionPayload(
                            drawingRevision: 0,
                            engine: "ios-notes-sync",
                            rawText: staged,
                            userCorrectedText: staged
                        )
                    )
                } catch {
                    // Not fatal: a run against the text the daemon already holds
                    // is better than no run, so the failure is swallowed here and
                    // the run below reports any real problem.
                    _ = error
                }
            }

            let run = try await client.createNoteRun(
                noteID: noteID,
                payload: ErisNoteRunCreatePayload(
                    clientRunId: clientMessageID.uuidString,
                    directives: ["enrich"]
                )
            )
            lastRunIDByLocalID[conversationID] = run.data.id
            continuation.yield(.started(phase: "Enriching"))

            var seen: Set<Int64> = []
            var accumulated = ""
            let deadline = ContinuousClock.now + Self.noteRunTimeout

            while ContinuousClock.now < deadline {
                if Task.isCancelled {
                    _ = try? await client.cancelNoteRun(runID: run.data.id)
                    continuation.yield(.cancelled)
                    continuation.finish()
                    return
                }
                let events = try await client.noteRunEvents(runID: run.data.id)
                for event in events where !seen.contains(event.seq) {
                    seen.insert(event.seq)
                    switch event.payload {
                    case .delta(let delta):
                        accumulated += delta.text
                        continuation.yield(.textDelta(delta.text))
                    case .failed(_, let message):
                        continuation.yield(.failed(message ?? "The enrichment failed on the daemon."))
                        continuation.finish()
                        return
                    case .cancelled:
                        continuation.yield(.cancelled)
                        continuation.finish()
                        return
                    case .completed, .started, .other:
                        break
                    }
                }
                let state = try await client.noteRun(runID: run.data.id)
                if state.data.isTerminal {
                    switch state.data.status {
                    case "completed":
                        let markdown = state.data.result?.markdown ?? accumulated
                        continuation.yield(.finished(
                            Message(
                                id: clientMessageID,
                                clientMessageID: clientMessageID,
                                sender: .herald,
                                content: markdown,
                                jobID: Self.stableUUID(from: run.data.id),
                                status: .sent
                            ),
                            nil,
                            nil,
                            nil
                        ))
                    case "cancelled":
                        continuation.yield(.cancelled)
                    default:
                        continuation.yield(.failed(
                            state.data.errorText ?? "The enrichment failed on the daemon."
                        ))
                    }
                    continuation.finish()
                    return
                }
                try await Task.sleep(for: Self.noteRunPollInterval)
            }
            continuation.yield(.failed("The enrichment ran past the client's own patience; it may still finish on the daemon."))
            continuation.finish()
        } catch {
            continuation.yield(.failed(error.localizedDescription))
            continuation.finish()
        }
    }

    /// The daemon's note id for a local note UUID.
    ///
    /// Notes are matched by `stableUUID(from:)` so a note created on the phone and
    /// the same note read back from the daemon resolve to one id. A note the
    /// daemon does not hold is created there, which is what makes a note written
    /// offline syncable at all.
    private func resolveNoteID(_ localID: UUID, client: ErisClient) async throws -> String {
        if let cached = erisNoteIDByLocalID[localID] { return cached }
        let list = try await client.notes()
        if let match = list.data.first(where: { Self.stableUUID(from: $0.id) == localID }) {
            erisNoteIDByLocalID[localID] = match.id
            return match.id
        }
        let title = notesTitle(for: localID)
        let created = try await client.createNote(ErisNoteCreatePayload(title: title))
        erisNoteIDByLocalID[localID] = created.data.id
        return created.data.id
    }

    /// The title to give a note the daemon is seeing for the first time.
    ///
    /// The sync engine owns the note's own title; the client only needs something
    /// better than an empty string, and an empty title is what the daemon
    /// defaults to anyway.
    private func notesTitle(for localID: UUID) -> String {
        "Note \(localID.uuidString.prefix(8))"
    }

    /// Pull the note's own text out of the sync prompt.
    ///
    /// The sync engine composes one message carrying the typed text, the OCR
    /// draft, the directive policy and an isolation rule. The daemon's worker does
    /// not want the policy - it has its own system message - so this takes the two
    /// labelled lines the engine wrote and nothing else. A prompt in another shape
    /// contributes nothing rather than the whole thing, because a run whose source
    /// is a wall of policy is worse than a run with no staged text.
    static func contentForRecognition(_ prompt: String) -> String {
        var lines: [String] = []
        for line in prompt.split(separator: "\n", omittingEmptySubsequences: false) {
            let text = String(line)
            if text.hasPrefix("Typed text:") || text.hasPrefix("Recognized text:") {
                lines.append(text)
            }
        }
        let joined = lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        return joined.count > 12_000 ? String(joined.prefix(12_000)) : joined
    }

    /// The run that last enriched a note, so the note records a key that exists.
    func nativeSessionKey(for conversationID: UUID) async -> String? {
        lastRunIDByLocalID[conversationID]
    }

    /// A note's "session" is its newest run. There is nothing to resume: the run
    /// is already over, and asking for it again is the sync engine's `.synced`
    /// checkpoint doing its job.
    func resumeNoteSession(conversationID: UUID, sessionKey: String) async -> Bool {
        lastRunIDByLocalID[conversationID] = sessionKey
        return true
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

    /// Open a row for the tool call about to run, and remember its id.
    private func openToolRun(named name: String) {
        toolRunSequence += 1
        activeToolRunID = "eris-tool-\(toolRunSequence)-\(name)"
    }

    private func currentConversationID() -> String? {
        guard let currentConversation else { return nil }
        return conversationIDByLocalID[currentConversation.id]
    }

    /// The local UUID for a daemon conversation.
    ///
    /// Derived from the daemon's id rather than minted, so one conversation has
    /// one identity no matter which path names it and no matter how many times
    /// the app relaunches. The daemon issues UUID-shaped ids, so this is the id
    /// itself; anything else falls back to `stableUUID`.
    private func localID(forConversation conversationID: String) -> UUID {
        if let existing = localIDByConversationID[conversationID] { return existing }
        let local = Self.stableUUID(from: conversationID)
        bind(localID: local, to: conversationID)
        return local
    }

    /// Record both directions of one binding, so `currentConversationID()` and
    /// `loadConversation(id:)` cannot disagree about which conversation a local
    /// id means, and remember it, so the agreement survives a relaunch.
    private func bind(localID: UUID, to conversationID: String) {
        conversationIDByLocalID[localID] = conversationID
        localIDByConversationID[conversationID] = localID
        persistBindings()
    }

    /// Recover the bindings the last launch recorded.
    ///
    /// The map used to be memory-only: a chat started before a relaunch had no
    /// binding to resolve against, so the app opened it blank and reported the
    /// conversation as gone. Restoring it means the id the app persisted as the
    /// open session still names its conversation on the next launch.
    private func restoreBindings() {
        guard let raw = defaults.dictionary(forKey: Self.bindingsKey) as? [String: String] else {
            return
        }
        for (conversationID, localText) in raw {
            guard let localID = UUID(uuidString: localText) else { continue }
            conversationIDByLocalID[localID] = conversationID
            localIDByConversationID[conversationID] = localID
        }
    }

    private func persistBindings() {
        var raw: [String: String] = [:]
        raw.reserveCapacity(localIDByConversationID.count)
        for (conversationID, localID) in localIDByConversationID {
            raw[conversationID] = localID.uuidString
        }
        defaults.set(raw, forKey: Self.bindingsKey)
    }

    /// Resolve a local id to the daemon's conversation id, healing a binding the
    /// client does not hold yet.
    ///
    /// The conversation list is the authority on which conversations exist, and
    /// every row on it resolves to a stable local id, so a miss is answered from
    /// the list instead of becoming an empty chat. An id that is not on the list
    /// is genuinely gone, and says so.
    private func resolveConversationID(for localID: UUID, client: ErisClient) async throws -> String {
        if let known = conversationIDByLocalID[localID] { return known }
        let page = try await client.conversations()
        for row in page.conversations where Self.stableUUID(from: row.id) == localID {
            bind(localID: localID, to: row.id)
            return row.id
        }
        throw ErisHeraldError.unknownSession
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

    /// One daemon transcript row as the client's own message type.
    ///
    /// Three fields matter beyond the text, and all three were previously left
    /// nil, which is what made the transcript unmergeable:
    ///
    /// - `clientMessageID` is the id the app minted when the operator hit send,
    ///   echoed back by the daemon. Without it the local optimistic row and the
    ///   daemon's row have nothing in common to match on, `mergeConversationMetadata`
    ///   falls through every branch, and the local row is dropped and redrawn -
    ///   the disappearing first message and the duplicated pairs.
    /// - `jobID` lets the merged rows be grouped by turn instead of by guess.
    /// - `timestamp` is the row's real `created_at`. Without it each refresh
    ///   restamped the row to "now", which is the spacing and the reordering.
    static func message(from m: ErisConversationMessage) -> Message {
        Message(
            id: stableUUID(from: m.id),
            clientMessageID: m.clientMessageID.flatMap(UUID.init(uuidString:)),
            sender: m.isUser ? .user : .herald,
            content: m.body,
            timestamp: ErisTimestamp.date(from: m.createdAt) ?? .now,
            jobID: m.jobID.flatMap { stableUUID(from: $0) },
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

    /// One row of the session list, keyed by the SAME local id the transcript
    /// loader and the turn path resolve.
    ///
    /// This used to be a static `stableUUID` projection that never registered
    /// the binding, so a row rendered here named an id nothing else knew: tapping
    /// it loaded an empty chat, and sending from it filed the turn in a
    /// conversation of its own. Registering through `localID(forConversation:)`
    /// is what makes the row, the transcript and the send one identity.
    func summary(from s: ErisConversationSummary) -> SessionSummary {
        SessionSummary(
            id: localID(forConversation: s.id),
            title: s.displayTitle,
            previewText: "",
            lastActivity: ErisTimestamp.date(from: s.updatedAt) ?? .now,
            sessionKey: s.id
        )
    }
}
