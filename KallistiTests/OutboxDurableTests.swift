import Foundation
import Testing
@testable import Kallisti

/// Build 33 Workstream B: durable outbox + visible attempt ownership.
///
/// Covers:
/// - Phase 1 enqueue appends the optimistic row immediately and persists
///   the outbox record to disk (survives relaunch)
/// - Full sendMessage drives the record to `.terminal` with canonical IDs
/// - FIFO chain: messages queued while a job streams submit only after the
///   active job reaches a terminal state
/// - Restart suspension: enqueued messages stay `.queued` until
///   `resumeAfterRestart()` drains them
/// - `recoverOutbox` settles `.accepted` jobs against the relay on relaunch
@MainActor
@Suite("B33 WSB durable outbox")
struct OutboxDurableTests {

    /// Submission order recorder. MainActor-only usage (the test and the
    /// streaming mock both run on the main actor), so no synchronization is
    /// needed.
    private final class OrderBox {
        private var _value: [String] = []
        var value: [String] { _value }
        func append(_ element: String) {
            _value.append(element)
        }
    }

    private func makeScratchDirectory() -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("outbox-tests-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func makePersistence() -> any AppPersistenceStoreProtocol {
        let suiteName = "outbox-tests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        return UserDefaultsAppPersistenceStore(defaults: defaults)
    }

    // MARK: - Phase 1: durable enqueue

    @Test("enqueueMessage appends the optimistic row immediately and persists the record")
    func enqueueAppendsOptimisticRowAndPersists() async throws {
        let client = MockHeraldClient()
        let persistence = makePersistence()
        let scratch = makeScratchDirectory()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: scratch)

        let clientMessageID = UUID()
        let record = store.enqueueMessage("Hello outbox", clientMessageID: clientMessageID)

        // Accepted and durably queued.
        let accepted = try #require(record)
        #expect(accepted.clientMessageID == clientMessageID)
        #expect(accepted.state == .queued)
        #expect(accepted.cleanText == "Hello outbox")

        // Optimistic row is visible BEFORE any network call — no conversation
        // was loaded and no server session was ensured.
        let optimistic = try #require(store.conversation?.messages.first(where: { $0.sender == .user }))
        #expect(optimistic.content == "Hello outbox")
        #expect(optimistic.status == .sending)
        #expect(client.currentConversation == nil)

        // Persisted: a fresh store over the same directory sees the record.
        let manifest = OutboxManifestStore(baseDirectory: scratch).load()
        #expect(manifest.items.contains { $0.clientMessageID == clientMessageID })
    }

    @Test("enqueueMessage rejects duplicates while the optimistic row is sending")
    func enqueueRejectsDuplicateWhileSending() {
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: persistence, outboxBaseDirectory: makeScratchDirectory())

        let first = store.enqueueMessage("Same text")
        #expect(first != nil)

        // Identical text while the first row is still .sending → rejected.
        let second = store.enqueueMessage("Same text")
        #expect(second == nil)
        #expect(store.outboxItems.count == 1)
    }

    // MARK: - Phase 2: submit

    @Test("sendMessage drives the record to terminal with canonical IDs")
    func sendMessageTerminalizesRecord() async throws {
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: persistence, outboxBaseDirectory: makeScratchDirectory())

        await store.sendMessage("Hello outbox")

        let record = try #require(store.outboxItems.first)
        #expect(record.state == .terminal)
        #expect(record.canonicalUserMessageID == record.clientMessageID)
        #expect(record.terminalMessageID != nil)
        #expect(record.attemptCount == 1)
        #expect(record.lastError == nil)
        // The optimistic row reached delivered.
        let userRow = try #require(store.conversation?.messages.first(where: { $0.sender == .user }))
        #expect(userRow.status == .delivered)
    }

    // MARK: - FIFO chain

    @Test("messages queued while a job streams submit FIFO after it finishes")
    func queuedItemsSubmitAfterActiveJob() async throws {
        // Short timeouts so the watchdog/polling overhead doesn't dominate.
        let origWatchdog = ChatStore.watchdogTimeout
        let origDeadline = ChatStore.absoluteJobDeadline
        ChatStore.watchdogTimeout = .milliseconds(500)
        ChatStore.absoluteJobDeadline = .seconds(2)
        defer { ChatStore.watchdogTimeout = origWatchdog; ChatStore.absoluteJobDeadline = origDeadline }

        final class SlowStreamClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?
            let order = OrderBox()

            func connect() async {}
            func disconnect() async {}

            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }

            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) -> AsyncStream<StreamingUpdate> {
                order.append(message)
                return AsyncStream { continuation in
                    Task { @MainActor in
                        continuation.yield(.messageSent(jobID: UUID()))
                        try? await Task.sleep(for: .milliseconds(100))
                        continuation.yield(.finished(
                            Message(sender: .herald, content: "reply to \(message)", status: .delivered),
                            nil, nil, nil
                        ))
                        continuation.finish()
                    }
                }
            }

            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func ensureConversation(id: UUID) async -> Bool { true }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let client = SlowStreamClient()
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: makeScratchDirectory())
        store.useStreaming = true

        let first = Task { await store.sendMessage("First") }
        try? await Task.sleep(for: .milliseconds(30))

        // Queued while the first job is still streaming — must NOT be
        // submitted concurrently (FIFO: one in-flight job per conversation).
        store.queueNextMessage(text: "Second", attachments: [])
        store.queueNextMessage(text: "Third", attachments: [])
        try? await Task.sleep(for: .milliseconds(30))
        #expect(client.order.value == ["First"], "queued items must wait behind the active job")

        await first.value
        // Let the FIFO chain drain the two queued items (~100ms each).
        try? await Task.sleep(for: .milliseconds(500))

        #expect(client.order.value == ["First", "Second", "Third"])
        #expect(store.outboxItems.filter { $0.state == .terminal }.count == 3)
        #expect(store.outboxItems.filter { $0.state == .queued }.isEmpty)
    }

    // MARK: - Restart suspension

    @Test("messages enqueued during a restart stay queued and submit after resume")
    func restartQueuesThenResumes() async throws {
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: persistence, outboxBaseDirectory: makeScratchDirectory())

        store.beginRestartSuspension()
        #expect(store.restartInProgress)

        let record = store.enqueueMessage("During restart")
        let accepted = try #require(record)
        #expect(accepted.state == .queued)

        // Phase 2 no-ops while suspended — no network, no state change.
        await store.submitNextEligible()
        #expect(store.outboxItems.first?.state == .queued)

        await store.resumeAfterRestart()
        #expect(store.restartInProgress == false)
        #expect(store.outboxItems.first?.state == .terminal)
    }

    // MARK: - Recovery

    @Test("recoverOutbox settles accepted jobs against the relay on relaunch")
    func recoverySettlesAcceptedJob() async throws {
        let jobID = UUID()
        let client = JobStatusClient(jobID: jobID, reportedStatus: "completed")
        let persistence = makePersistence()
        let scratch = makeScratchDirectory()

        // Simulate a force-quit mid-job: an accepted record with a jobID,
        // persisted by a previous process.
        let clientMessageID = UUID()
        let conversationID = UUID()
        let record = ChatOutboxRecord(
            schemaVersion: OutboxManifestStore.schemaVersion,
            clientMessageID: clientMessageID,
            conversationID: conversationID,
            createdAt: .now,
            sequence: 1,
            cleanText: "Sent before relaunch",
            continuationContext: nil,
            attachmentRefs: [],
            state: .accepted,
            jobID: jobID,
            canonicalUserMessageID: clientMessageID,
            terminalMessageID: nil,
            attemptCount: 1,
            nextAttemptAt: nil,
            lastError: nil,
            updatedAt: .now
        )
        OutboxManifestStore(baseDirectory: scratch).save(
            ChatOutboxManifest(schemaVersion: OutboxManifestStore.schemaVersion, items: [record], nextSequence: 2)
        )

        // "Relaunch": a fresh store over the same directory.
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: scratch)
        #expect(store.outboxItems.count == 1)

        await store.recoverOutbox()

        let settled = try #require(store.outboxItems.first)
        #expect(settled.state == .terminal)
        #expect(settled.terminalMessageID != nil)
    }

    @Test("recoverOutbox marks failed relay jobs retryable, unknown jobs manual-only")
    func recoveryClassifiesJobFailures() async throws {
        let failedJobID = UUID()
        let unknownJobID = UUID()
        let failedClient = JobStatusClient(jobID: failedJobID, reportedStatus: "failed")
        let persistence = makePersistence()
        let scratch = makeScratchDirectory()

        func acceptedRecord(_ clientMessageID: UUID, _ conversationID: UUID, _ jobID: UUID, sequence: Int) -> ChatOutboxRecord {
            ChatOutboxRecord(
                schemaVersion: OutboxManifestStore.schemaVersion,
                clientMessageID: clientMessageID,
                conversationID: conversationID,
                createdAt: .now,
                sequence: sequence,
                cleanText: "message",
                continuationContext: nil,
                attachmentRefs: [],
                state: .accepted,
                jobID: jobID,
                canonicalUserMessageID: clientMessageID,
                terminalMessageID: nil,
                attemptCount: 1,
                nextAttemptAt: nil,
                lastError: nil,
                updatedAt: .now
            )
        }

        let knownID = UUID()
        let unknownID = UUID()
        let knownConv = UUID()
        let unknownConv = UUID()
        let known = acceptedRecord(knownID, knownConv, failedJobID, sequence: 1)
        let unknown = acceptedRecord(unknownID, unknownConv, unknownJobID, sequence: 2)
        OutboxManifestStore(baseDirectory: scratch).save(
            ChatOutboxManifest(
                schemaVersion: OutboxManifestStore.schemaVersion,
                items: [known, unknown],
                nextSequence: 3
            )
        )

        let store = ChatStore(heraldClient: failedClient, persistence: persistence, outboxBaseDirectory: scratch)
        await store.recoverOutbox()

        // Relay-reported failure → retryable with a scheduled backoff.
        let knownRecord = try #require(store.outboxItems.first { $0.clientMessageID == knownID })
        #expect(knownRecord.state == .retryableFailure)
        #expect(knownRecord.nextAttemptAt != nil, "relay-reported failures should auto-retry after backoff")

        // Unknown job → surfaced, never auto-resubmitted (manual retry only).
        let unknownRecord = try #require(store.outboxItems.first { $0.clientMessageID == unknownID })
        #expect(unknownRecord.state == .retryableFailure)
        #expect(unknownRecord.nextAttemptAt == nil, "ambiguous jobs must not auto-resubmit")
    }

    // MARK: - Follow-up wedge fix (Build 7)

    /// Helper: a mock client whose `sendStreaming` yields `.messageSent(jobID:)`
    /// then finishes WITHOUT `.finished` (terminalless), simulating a hung
    /// stream. `getJobStatus` is configurable per test.
    @MainActor
    private final class HungStreamClient: HeraldClientProtocol {
        var connectionStatus: ConnectionStatus = .connected
        var currentConversation: Conversation?
        let order = OrderBox()
        let jobStatusProvider: (UUID) async -> LiveHeraldClient.JobStatusResponse?

        init(jobStatusProvider: @escaping (UUID) async -> LiveHeraldClient.JobStatusResponse?) {
            self.jobStatusProvider = jobStatusProvider
        }

        func connect() async {}
        func disconnect() async {}

        func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) async -> Message {
            Message(sender: .herald, content: "unused", status: .delivered)
        }

        func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) -> AsyncStream<StreamingUpdate> {
            order.append(message)
            return AsyncStream { continuation in
                Task { @MainActor in
                    let jobID = UUID()
                    continuation.yield(.messageSent(jobID: jobID))
                    // NO .finished — simulates a terminalless stream.
                    continuation.finish()
                }
            }
        }

        func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
        func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
        func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
        func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
        func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
        func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
        func ensureConversation(id: UUID) async -> Bool { true }
        func deleteSession(id: UUID) async throws {}
        func archiveSession(id: UUID) async throws {}
        func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
        func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
        func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
        func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
        func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
            await jobStatusProvider(jobId)
        }
        func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
            Message(sender: .herald, content: text, status: .delivered)
        }
        func cancelJob(jobID: UUID) async throws {}
    }

    @Test("hung stream settles from job status, FIFO drains (the follow-up wedge bug)")
    func hungStreamSettlesFromJobStatusFIFODrains() async throws {
        // Use short timeouts so the test completes in seconds, not minutes.
        let origWatchdog = ChatStore.watchdogTimeout
        let origDeadline = ChatStore.absoluteJobDeadline
        ChatStore.watchdogTimeout = .milliseconds(500)
        ChatStore.absoluteJobDeadline = .seconds(2)
        defer { ChatStore.watchdogTimeout = origWatchdog; ChatStore.absoluteJobDeadline = origDeadline }

        let client = HungStreamClient { _ in
            LiveHeraldClient.JobStatusResponse(
                status: "completed",
                conversationId: nil,
                message: Message(sender: .herald, content: "reply", status: .delivered),
                error: nil, usage: nil, context: nil, diff: nil, attempt: nil, lastSeq: nil, errorCategory: nil, errorAction: nil
            )
        }
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: makeScratchDirectory())
        store.useStreaming = true

        let first = Task { await store.sendMessage("First") }
        try? await Task.sleep(for: .milliseconds(30))

        // Queue while the first job is in flight (hung stream, no terminal).
        store.queueNextMessage(text: "Second", attachments: [])

        // Let the first attempt run and settle.
        await first.value
        try? await Task.sleep(for: .milliseconds(500))

        #expect(client.order.value == ["First", "Second"], "Second must be submitted after First settles")
        #expect(store.outboxItems.filter { $0.state == .terminal }.count == 2, "Both items must reach terminal")
        #expect(store.outboxItems.filter { $0.state == .accepted }.isEmpty, "No item left stuck in accepted")
    }

    @Test("hung stream, job status unavailable → retryable, FIFO still drains")
    func hungStreamJobStatusUnavailableFIFODrains() async throws {
        let origWatchdog = ChatStore.watchdogTimeout
        let origDeadline = ChatStore.absoluteJobDeadline
        ChatStore.watchdogTimeout = .milliseconds(500)
        ChatStore.absoluteJobDeadline = .seconds(2)
        defer { ChatStore.watchdogTimeout = origWatchdog; ChatStore.absoluteJobDeadline = origDeadline }

        let client = HungStreamClient { _ in nil }  // getJobStatus returns nil
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: makeScratchDirectory())
        store.useStreaming = true

        let first = Task { await store.sendMessage("First") }
        try? await Task.sleep(for: .milliseconds(30))

        store.queueNextMessage(text: "Second", attachments: [])

        await first.value
        try? await Task.sleep(for: .milliseconds(500))

        // First must NOT be left .accepted — nil means we can't confirm
        // status, so it must move to retryableFailure to free the FIFO.
        let firstItem = try #require(store.outboxItems.first(where: { $0.cleanText == "First" }))
        #expect(firstItem.state != .accepted, "nil getJobStatus must not leave item .accepted")

        // Second must have been submitted (FIFO drained). It may also
        // have cycled through retryableFailure if getJobStatus stayed nil,
        // so check that "Second" appeared in the submission order at least once.
        #expect(client.order.value.contains("Second"), "Second must submit after First is freed")
    }

    @Test("genuinely running job is not prematurely settled before the deadline")
    func runningJobKeepsLease() async throws {
        let origWatchdog = ChatStore.watchdogTimeout
        let origDeadline = ChatStore.absoluteJobDeadline
        ChatStore.watchdogTimeout = .milliseconds(500)
        ChatStore.absoluteJobDeadline = .seconds(2)
        defer { ChatStore.watchdogTimeout = origWatchdog; ChatStore.absoluteJobDeadline = origDeadline }

        // Track how many times getJobStatus was called so we can verify
        // the settle function ran (and saw "running") before the deadline.
        actor CallCounter { var count = 0; func increment() { count += 1 } }
        let counter = CallCounter()

        let client = HungStreamClient { _ in
            await counter.increment()
            return LiveHeraldClient.JobStatusResponse(
                status: "running",
                conversationId: nil, message: nil, error: nil, usage: nil, context: nil, diff: nil, attempt: nil, lastSeq: nil, errorCategory: nil, errorAction: nil
            )
        }
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: makeScratchDirectory())
        store.useStreaming = true

        let first = Task { await store.sendMessage("First") }
        try? await Task.sleep(for: .milliseconds(30))

        store.queueNextMessage(text: "Second", attachments: [])

        await first.value
        try? await Task.sleep(for: .milliseconds(500))

        // The settle function ran and saw "running" — getJobStatus was
        // called at least once from the polling loop.
        let callCount = await counter.count
        #expect(callCount >= 1, "settleAcceptedOutboxJob must query getJobStatus in the polling loop")

        // After the absolute deadline, the item is forced to retryable.
        let firstItem = try #require(store.outboxItems.first(where: { $0.cleanText == "First" }))
        #expect(firstItem.state == .retryableFailure, "deadline forces running job to retryableFailure")

        // After the deadline settles First, the FIFO drains Second.
        #expect(client.order.value.contains("Second"), "FIFO must drain Second after deadline settles First")
    }
}

/// Minimal client with a scripted job-status response, for recovery tests.
@MainActor
private final class JobStatusClient: HeraldClientProtocol {
    var connectionStatus: ConnectionStatus = .connected
    var currentConversation: Conversation?
    private let jobID: UUID
    private let reportedStatus: String

    init(jobID: UUID, reportedStatus: String) {
        self.jobID = jobID
        self.reportedStatus = reportedStatus
    }

    func connect() async {}
    func disconnect() async {}
    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) async -> Message {
        Message(sender: .herald, content: "reply", status: .delivered)
    }
    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) -> AsyncStream<StreamingUpdate> {
        AsyncStream { $0.finish() }
    }
    func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Herald") }
    func clearConversation() async throws -> Conversation { Conversation(title: "Herald") }
    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Herald") }
    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
    func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
    func ensureConversation(id: UUID) async -> Bool { true }
    func deleteSession(id: UUID) async throws {}
    func archiveSession(id: UUID) async throws {}
    func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
    func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
    func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }
    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
        guard jobId == jobID else { return nil }
        return LiveHeraldClient.JobStatusResponse(
            status: reportedStatus,
            conversationId: nil,
            message: reportedStatus == "completed"
                ? Message(sender: .herald, content: "Completed reply", status: .delivered)
                : nil,
            error: reportedStatus == "failed" ? "The job failed" : nil,
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
        Message(sender: .user, content: text, status: .sent)
    }
    func cancelJob(jobID: UUID) async throws {}
}
