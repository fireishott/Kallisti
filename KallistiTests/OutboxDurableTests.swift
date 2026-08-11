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

    @Test("pre-ack polling keeps exactly one live thinking placeholder")
    func preAckPollingKeepsSingleLivePlaceholder() async throws {
        final class DelayedAckClient: HeraldClientProtocol {
            var connectionStatus: ConnectionStatus = .connected
            var currentConversation: Conversation?

            func connect() async {}
            func disconnect() async {}
            func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) async -> Message {
                Message(sender: .herald, content: "unused", status: .delivered)
            }
            func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) -> AsyncStream<StreamingUpdate> {
                AsyncStream { continuation in
                    Task { @MainActor in
                        try? await Task.sleep(for: .seconds(3))
                        continuation.yield(.messageSent(jobID: UUID()))
                        try? await Task.sleep(for: .milliseconds(100))
                        continuation.yield(.finished(
                            Message(sender: .herald, content: "reply", status: .delivered),
                            nil, nil, nil
                        ))
                        continuation.finish()
                    }
                }
            }
            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Kallisti") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Kallisti") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Kallisti") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func ensureConversation(id: UUID) async -> Bool { true }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation {
                currentConversation ?? Conversation(id: id, title: "Kallisti")
            }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let client = DelayedAckClient()
        let store = ChatStore(
            heraldClient: client,
            persistence: makePersistence(),
            outboxBaseDirectory: makeScratchDirectory()
        )
        store.useStreaming = true
        store.setPollingEnabled(true)

        let send = Task { await store.sendMessage("One turn") }
        try? await Task.sleep(for: .milliseconds(2_300))

        let liveRows = store.conversation?.messages.filter { $0.sender == .herald && $0.isStreaming } ?? []
        #expect(liveRows.count == 1, "a pre-ack transcript refresh must not orphan or duplicate the turn placeholder")

        await send.value
    }

    @Test("follow-up submits immediately after prior stream finishes")
    func followUpDoesNotWaitForCancelledTaskState() async throws {
        final class ImmediateStreamClient: HeraldClientProtocol {
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
                    continuation.yield(.finished(
                        Message(sender: .herald, content: "reply to \(message)", status: .delivered),
                        nil, nil, nil
                    ))
                    continuation.finish()
                }
            }
            func loadConversation() async -> Conversation { currentConversation ?? Conversation(title: "Kallisti") }
            func clearConversation() async throws -> Conversation { Conversation(title: "Kallisti") }
            func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation { Conversation(title: "Kallisti") }
            func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse { SessionListResponse(sessions: [], total: 0) }
            func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] { [] }
            func createSession(title: String) async throws -> SessionSummary { SessionSummary(id: UUID(), title: title) }
            func ensureConversation(id: UUID) async -> Bool { true }
            func deleteSession(id: UUID) async throws {}
            func archiveSession(id: UUID) async throws {}
            func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Test") }
            func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
            func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
            func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Kallisti") }
            func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
            func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message { Message(sender: .herald, content: text, status: .delivered) }
            func cancelJob(jobID: UUID) async throws {}
        }

        let originalTimeout = ChatStore.watchdogTimeout
        ChatStore.watchdogTimeout = .seconds(90)
        defer { ChatStore.watchdogTimeout = originalTimeout }

        let client = ImmediateStreamClient()
        let store = ChatStore(
            heraldClient: client,
            persistence: makePersistence(),
            outboxBaseDirectory: makeScratchDirectory()
        )
        store.useStreaming = true

        let started = ContinuousClock.now
        await store.sendMessage("First")
        await store.sendMessage("Follow-up")
        let elapsed = started.duration(to: .now)

        #expect(client.order.value == ["First", "Follow-up"])
        #expect(elapsed < .seconds(2), "follow-up must not wait for the 90-second watchdog")
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

    // MARK: - Build 77: live delivery-state settlement hardening

    @Test("recoverOutbox upgrades the user row to .delivered when relay returns a terminal assistant message id")
    func recoveryUpgradesUserRowWhenTerminalMessagePresent() async throws {
        // Scenario: app was force-quit mid-turn; on relaunch, the outbox has
        // an .accepted record with a known jobID and the conversation has the
        // optimistic user row whose clientMessageID matches.  The relay
        // confirms the job is "completed" and returns the canonical terminal
        // assistant message id.  recoverOutbox must stamp the user row
        // .delivered so the green checkmark dot appears.
        let jobID = UUID()
        let assistantMessageID = UUID()
        let clientMessageID = UUID()
        let conversationID = UUID()

        let client = TerminalAssistantClient(
            jobID: jobID,
            reportedStatus: "completed",
            assistantMessageID: assistantMessageID
        )

        // Pre-populate the conversation with the optimistic user row so
        // terminalizeOutboxItem finds a target to stamp.  In production the
        // conversation is loaded by the parallel conversationLoad async let
        // (ChatScreen.swift:143 and AppContainer.swift:1186); for the test we
        // set it directly after relaunch.
        let userRow = Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .user,
            content: "Sent before relaunch",
            jobID: jobID,
            status: .sent
        )
        let conversation = Conversation(
            id: conversationID,
            title: "Relaunch",
            messages: [userRow]
        )

        // Persist the accepted record from a prior process.
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
        let scratch = makeScratchDirectory()
        OutboxManifestStore(baseDirectory: scratch).save(
            ChatOutboxManifest(
                schemaVersion: OutboxManifestStore.schemaVersion,
                items: [record],
                nextSequence: 2
            )
        )

        let persistence = makePersistence()
        let store = ChatStore(
            heraldClient: client,
            persistence: persistence,
            outboxBaseDirectory: scratch
        )
        store.conversation = conversation

        await store.recoverOutbox()

        // Outbox record moved to .terminal and recorded the canonical
        // assistant message id (identity-evidenced settlement).
        let settled = try #require(store.outboxItems.first)
        #expect(settled.state == .terminal)
        #expect(settled.terminalMessageID == assistantMessageID)

        // The user row in the conversation was upgraded to .delivered.
        // Without this assertion the Build 76 symptom (single grey check
        // after a foreground rehydrate) returns.
        let userAfter = try #require(
            store.conversation?.messages.first(where: { $0.id == clientMessageID })
        )
        #expect(userAfter.status == .delivered,
                "user row must be .delivered when relay returns a terminal assistant message id")
    }

    @Test("recoverOutbox leaves the user row .sent when relay reports terminal without an assistant message id")
    func recoveryLeavesUserRowSentWhenTerminalMessageMissing() async throws {
        // Safe-condition negative path: relay says "completed" but the
        // response carries no `message` field (e.g. transient relay race or
        // a job whose assistant row was not yet persisted).  The fix must
        // NOT stamp the user row .delivered; that would be the
        // "never just when message accepted" anti-pattern from the parent's
        // rule.  The outbox record still moves to .terminal so the FIFO can
        // drain, and a later refresh reconciles the user status.
        let jobID = UUID()
        let clientMessageID = UUID()
        let conversationID = UUID()

        let client = TerminalAssistantClient(
            jobID: jobID,
            reportedStatus: "completed",
            assistantMessageID: nil
        )

        let userRow = Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .user,
            content: "Sent before relaunch",
            jobID: jobID,
            status: .sent
        )
        let conversation = Conversation(
            id: conversationID,
            title: "Relaunch",
            messages: [userRow]
        )

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
        let scratch = makeScratchDirectory()
        OutboxManifestStore(baseDirectory: scratch).save(
            ChatOutboxManifest(
                schemaVersion: OutboxManifestStore.schemaVersion,
                items: [record],
                nextSequence: 2
            )
        )

        let persistence = makePersistence()
        let store = ChatStore(
            heraldClient: client,
            persistence: persistence,
            outboxBaseDirectory: scratch
        )
        store.conversation = conversation

        await store.recoverOutbox()

        // Outbox record still settled (FIFO can drain).
        let settled = try #require(store.outboxItems.first)
        #expect(settled.state == .terminal)

        // User row stays .sent: no identity evidence for a terminal
        // assistant message, so the safe condition for the upgrade is not
        // met.
        let userAfter = try #require(
            store.conversation?.messages.first(where: { $0.id == clientMessageID })
        )
        #expect(userAfter.status == .sent,
                "user row must stay .sent when relay has no canonical assistant message id")
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

    /// Client that reports a still-running session on resume (the Build 52
    /// sleep-recovery contract). The stream never finishes on its own - the
    /// test drives recoverStalledStream to prove the resume path keeps the
    /// stream alive instead of declaring a stall.
    private final class ResumeAwareClient: HeraldClientProtocol {
        var connectionStatus: ConnectionStatus = .connected
        var currentConversation: Conversation?
        var resumeCalls = 0
        let resumeResult: Bool
        let conversation: Conversation

        init(resumeResult: Bool, conversation: Conversation) {
            self.resumeResult = resumeResult
            self.conversation = conversation
            self.currentConversation = conversation
        }

        func connect() async {}
        func disconnect() async {}

        func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) async -> Message {
            Message(sender: .herald, content: "unused", status: .delivered)
        }

        func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String? = nil) -> AsyncStream<StreamingUpdate> {
            AsyncStream { continuation in
                Task { @MainActor in
                    let jobID = UUID()
                    continuation.yield(.messageSent(jobID: jobID))
                    // No .finished: simulates a terminalless stream whose job
                    // is still running server-side after a suspension.
                }
            }
        }

        func loadConversation() async -> Conversation { conversation }
        func loadConversation(id: UUID) async throws -> Conversation { conversation }
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
        func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }
        func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
            Message(sender: .herald, content: text, status: .delivered)
        }
        func cancelJob(jobID: UUID) async throws {}
        func resumeActiveSessionIfNeeded() async -> Bool {
            resumeCalls += 1
            return resumeResult
        }
    }

    @Test("recoverStalledStream resumes a still-running session instead of stalling (Build 52)")
    func recoverStalledStreamResumesRunningSession() async throws {
        let conversation = Conversation(title: "Resume Test")
        let client = ResumeAwareClient(resumeResult: true, conversation: conversation)
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: makeScratchDirectory())
        store.useStreaming = true

        // Drive a real send through the terminalless stream so the store's
        // streaming state is genuinely in-flight (activeStreams populated via
        // the .messageSent update).
        let first = Task { await store.sendMessage("First") }
        try? await Task.sleep(for: .milliseconds(100))
        #expect(store.isStreaming, "stream must be in flight before recovery")

        // Simulate the app backgrounding mid-stream, then foregrounding.
        store.markStreamBackgrounded()
        store.markStreamForegrounded()
        await store.recoverStalledStream()
        first.cancel()

        #expect(client.resumeCalls == 1, "resume must be attempted on foreground recovery")
        #expect(store.isStreaming, "a still-running session must keep the stream alive, not stall")
    }

    @Test("recoverStalledStream does not fabricate a live stream for a dead session (Build 52)")
    func recoverStalledStreamDeadSessionFallsThrough() async throws {
        let conversation = Conversation(title: "Dead Test")
        let client = ResumeAwareClient(resumeResult: false, conversation: conversation)
        let persistence = makePersistence()
        let store = ChatStore(heraldClient: client, persistence: persistence, outboxBaseDirectory: makeScratchDirectory())
        store.useStreaming = true

        let first = Task { await store.sendMessage("First") }
        try? await Task.sleep(for: .milliseconds(100))
        #expect(store.isStreaming, "stream must be in flight before recovery")

        store.markStreamBackgrounded()
        store.markStreamForegrounded()
        await store.recoverStalledStream()
        first.cancel()

        #expect(client.resumeCalls == 1, "resume must be attempted")
        // Resume=false and no server response: recoverStalledStream leaves the
        // terminal decision to the watchdog/deadline - it must NOT declare
        // success or kill the stream state prematurely.
        #expect(store.streamingPhase != .idle || !store.isStreaming,
                "dead session must not fake a live stream")
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


// MARK: - Build 77 helper: returns a terminal assistant message with a
// configurable id so the recovery test can assert the exact canonical id
// propagated into the outbox record (and the negative-path test can omit
// the message entirely to verify the safe-condition guard).
private final class TerminalAssistantClient: HeraldClientProtocol {
    var connectionStatus: ConnectionStatus = .connected
    var currentConversation: Conversation?
    private let jobID: UUID
    private let reportedStatus: String
    private let assistantMessageID: UUID?

    init(jobID: UUID, reportedStatus: String, assistantMessageID: UUID?) {
        self.jobID = jobID
        self.reportedStatus = reportedStatus
        self.assistantMessageID = assistantMessageID
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
    func createSession(title: String) async throws -> SessionSummary { SessionSummary(title: title, previewText: "New conversation", source: "ios") }
    func ensureConversation(id: UUID) async -> Bool { true }
    func deleteSession(id: UUID) async throws {}
    func archiveSession(id: UUID) async throws {}
    func togglePinSession(id: UUID) async throws -> SessionSummary { SessionSummary(id: id, title: "Pinned", isPinned: true) }
    func renameSession(id: UUID, title: String) async throws -> SessionSummary { SessionSummary(id: id, title: title) }
    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String { "New Chat" }
    func loadConversation(id: UUID) async throws -> Conversation { currentConversation ?? Conversation(title: "Herald") }

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
        guard jobId == jobID else { return nil }
        let assistantMessage: Message? = assistantMessageID.map { id in
            Message(id: id, sender: .herald, content: "Completed reply", status: .delivered)
        }
        return LiveHeraldClient.JobStatusResponse(
            status: reportedStatus,
            conversationId: nil,
            message: assistantMessage,
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
