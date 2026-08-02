import Foundation
import Testing
@testable import Herald

// MARK: - Test 1: Navigation race — late load for old conversation must not mutate active state

@Suite("Navigation race: stale load does not mutate active state")
struct NavigationRaceTests {
    @Test("late load for conversation A does not mutate conversation B's visible projection")
    func lateLoadDoesNotMutateActiveProjection() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let convA = CanonicalConversationID("conv-A")!
        let convB = CanonicalConversationID("conv-B")!

        // Navigate to A first via scope (epoch 1)
        await scope.navigateTo(convA) { _, _ in
            return TranscriptSnapshot(
                conversationID: convA,
                revision: ConversationRevision(rawValue: 1),
                canonicalMessages: [
                    CanonicalMessage(canonicalMessageID: CanonicalMessageID("can-a-1")!,
                        clientMessageID: nil, jobID: nil,
                        sequence: CanonicalSequence(rawValue: 1),
                        messageRevision: MessageRevision(rawValue: 1),
                        kind: .assistant, displayContent: "Hello from A", deleted: false)
                ])
        }
        // Wait for A's load
        try? await Task.sleep(nanoseconds: 50_000_000)

        // Navigate to B — this increments epoch to 2 and cancels old tasks
        await scope.navigateTo(convB) { capturedEpoch, capturedID in
            // Simulate a slow server response for B
            try? await Task.sleep(nanoseconds: 50_000_000) // 50ms

            return TranscriptSnapshot(
                conversationID: capturedID,
                revision: ConversationRevision(rawValue: 1),
                canonicalMessages: [
                    CanonicalMessage(canonicalMessageID: CanonicalMessageID("can-b-1")!,
                        clientMessageID: nil, jobID: nil,
                        sequence: CanonicalSequence(rawValue: 1),
                        messageRevision: MessageRevision(rawValue: 1),
                        kind: .assistant, displayContent: "Hello from B", deleted: false)
                ])
        }

        // Wait for B's load to finish
        try? await Task.sleep(nanoseconds: 100_000_000) // 100ms

        // Verify B is visible with B's content
        let projectionB = await scope.projection(for: convB)
        #expect(projectionB.activeConversationID == convB)
        #expect(projectionB.orderedVisibleRows.count == 1)
        #expect(projectionB.orderedVisibleRows[0].displayContent == "Hello from B")
    }

    @Test("navigating away cancels load tasks owned by the old conversation")
    func navigationCancelsOldLoad() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let convA = CanonicalConversationID("conv-A")!
        let convB = CanonicalConversationID("conv-B")!

        // Register a poll task that we can cancel via navigation
        let pollTask = Task { () -> Void in try? await Task.sleep(nanoseconds: 500_000_000) }
        await scope.setPollTask(pollTask)

        // Navigate to B — this should cancel the old poll task
        await scope.navigateTo(convB) { _, _ in
            return TranscriptSnapshot(
                conversationID: convB,
                revision: ConversationRevision(rawValue: 1),
                canonicalMessages: [])
        }

        // Verify the task was cancelled
        #expect(pollTask.isCancelled, "Poll task should be cancelled by navigation")
    }
}

// MARK: - Test 2: Late stream events from conversation A cannot cross-contaminate B

@Suite("Cross-conversation stream contamination prevention")
struct CrossConversationStreamTests {
    @Test("late stream delta for old conversation is rejected by reducer")
    func lateStreamDeltaRejected() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let convA = CanonicalConversationID("conv-A")!
        let convB = CanonicalConversationID("conv-B")!

        // Activate A
        _ = try await reducer.reduce(.activateConversation(convA, NavigationEpoch(rawValue: 1)))

        // Bind a job for A
        let jobID_A = JobID("job-A")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: convA, jobID: jobID_A,
                canonicalMessageID: nil, renderID: TranscriptRenderID(),
                displayContent: "A streaming", retryGeneration: nil),
            NavigationEpoch(rawValue: 1)))

        // Navigate to B (epoch 2)
        await scope.navigateTo(convB) { _, _ in
            return TranscriptSnapshot(conversationID: convB, revision: .zero)
        }

        // Try to deliver a late stream delta for A with epoch 1
        let rejected = await scope.isRejectedByReducer(
            .streamDelta(
                TranscriptStreamDelta(conversationID: convA, jobID: jobID_A,
                    canonicalMessageID: nil,
                    conversationRevision: ConversationRevision(rawValue: 1),
                    messageRevision: MessageRevision(rawValue: 1),
                    displayContent: "Contaminating text"),
                NavigationEpoch(rawValue: 1)))

        #expect(rejected, "Stale-epoch stream delta must be rejected by reducer")
    }

    @Test("late terminal event for old conversation is rejected by reducer")
    func lateTerminalEventRejected() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let convA = CanonicalConversationID("conv-A")!
        let convB = CanonicalConversationID("conv-B")!

        // Activate A
        _ = try await reducer.reduce(.activateConversation(convA, NavigationEpoch(rawValue: 1)))

        // Navigate to B
        await scope.navigateTo(convB) { _, _ in
            return TranscriptSnapshot(conversationID: convB, revision: .zero)
        }

        // Late terminal for A — must be rejected
        let rejected = await scope.isRejectedByReducer(
            .messageTerminal(
                TranscriptTerminalEvent(conversationID: convA, jobID: JobID("job-A")!,
                    canonicalMessageID: CanonicalMessageID("can-A")!,
                    conversationRevision: ConversationRevision(rawValue: 2),
                    messageRevision: MessageRevision(rawValue: 2),
                    displayContent: "Stale terminal", failure: nil),
                NavigationEpoch(rawValue: 1)))

        #expect(rejected, "Stale-epoch terminal event must be rejected")
    }
}

// MARK: - Test 3: Cancellation propagates — cancelling the scope cancels all owned tasks

@Suite("Cancellation propagation")
struct CancellationPropagationTests {
    @Test("teardown cancels all owned tasks")
    func teardownCancelsAllTasks() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        // Register tasks
        let pollTask = Task { () -> Void in try? await Task.sleep(nanoseconds: 500_000_000) }
        let cacheTask = Task { () -> Void in try? await Task.sleep(nanoseconds: 500_000_000) }
        let streamTask = Task { () -> Void in try? await Task.sleep(nanoseconds: 500_000_000) }

        await scope.setPollTask(pollTask)
        await scope.setCacheWriteTask(cacheTask)
        await scope.registerStreamTask(streamTask, for: JobID("job-1")!)

        // Teardown
        await scope.teardown()

        // All tasks must be cancelled
        #expect(pollTask.isCancelled, "Poll task should have been cancelled")
        #expect(cacheTask.isCancelled, "Cache write task should have been cancelled")
        #expect(streamTask.isCancelled, "Stream task should have been cancelled")
    }

    @Test("navigation transition cancels old tasks but not send tasks")
    func navigationCancelsOldButKeepsSends() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let pollTask = Task { () -> Void in try? await Task.sleep(nanoseconds: 500_000_000) }
        await scope.setPollTask(pollTask)

        // Register a send task — this should survive navigation
        let sendTask = Task { () -> Void in try? await Task.sleep(nanoseconds: 200_000_000) }
        await scope.registerSendTask(sendTask, for: ClientMessageID("client-1")!)

        // Navigate
        await scope.navigateTo(CanonicalConversationID("conv-B")!) { _, _ in
            return TranscriptSnapshot(conversationID: CanonicalConversationID("conv-B")!,
                revision: .zero)
        }

        #expect(pollTask.isCancelled, "Poll task should be cancelled by navigation")
        // Send task is NOT cancelled by navigation (queueing support)
        #expect(!sendTask.isCancelled, "Send task must survive navigation")
    }
}

// MARK: - Test 4: Per-await re-validation — result arriving after navigation must not be reduced

@Suite("Per-await re-validation")
struct PerAwaitRevalidationTests {
    @Test("snapshot loader result arriving after navigation is silently dropped")
    func staleLoadResultDropped() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let convA = CanonicalConversationID("conv-A")!
        let convB = CanonicalConversationID("conv-B")!

        // Activate A
        _ = try await reducer.reduce(.activateConversation(convA, NavigationEpoch(rawValue: 1)))

        // Start a slow load for A
        await scope.navigateTo(convA) { capturedEpoch, capturedID in
            // Simulate slow load
            try? await Task.sleep(nanoseconds: 200_000_000)

            // Return snapshot — the scope handles validation
            return TranscriptSnapshot(
                conversationID: capturedID,
                revision: ConversationRevision(rawValue: 1),
                canonicalMessages: [
                    CanonicalMessage(canonicalMessageID: CanonicalMessageID("stale-can")!,
                        clientMessageID: nil, jobID: nil,
                        sequence: CanonicalSequence(rawValue: 1),
                        messageRevision: MessageRevision(rawValue: 1),
                        kind: .assistant, displayContent: "Stale content", deleted: false)
                ])
        }

        // Navigate to B before A's load finishes
        try? await Task.sleep(nanoseconds: 50_000_000) // 50ms — A is still loading
        await scope.navigateTo(convB) { _, _ in
            return TranscriptSnapshot(
                conversationID: convB,
                revision: ConversationRevision(rawValue: 1),
                canonicalMessages: [
                    CanonicalMessage(canonicalMessageID: CanonicalMessageID("can-b-1")!,
                        clientMessageID: nil, jobID: nil,
                        sequence: CanonicalSequence(rawValue: 1),
                        messageRevision: MessageRevision(rawValue: 1),
                        kind: .assistant, displayContent: "Content from B", deleted: false)
                ])
        }

        // Wait for both loads to finish
        try? await Task.sleep(nanoseconds: 300_000_000)

        // B should be active with B's content, not A's stale content
        let projection = await scope.projection(for: convB)
        #expect(projection.activeConversationID == convB)
        #expect(projection.navigationEpoch == NavigationEpoch(rawValue: 2))

        // If A's stale content leaked in, this would fail
        for row in projection.orderedVisibleRows {
            #expect(row.displayContent != "Stale content",
                    "Stale content from conversation A must not appear in B's projection")
        }
    }
}

// MARK: - Test 5: Stale-epoch reducer rejection (defence in depth)

@Suite("Stale-epoch reducer rejection: defence in depth")
struct StaleEpochRejectionTests {
    @Test("events with pre-navigation epoch are rejected by the reducer")
    func staleEpochEventsRejected() async throws {
        let reducer = TranscriptReducer()

        let convA = CanonicalConversationID("conv-A")!

        // Activate at epoch 1
        _ = try await reducer.reduce(.activateConversation(convA, NavigationEpoch(rawValue: 1)))

        // Navigate away (epoch 2)
        _ = try await reducer.reduce(.activateConversation(convA, NavigationEpoch(rawValue: 2)))

        // Submit snapshot with epoch 1 — must be rejected
        do {
            _ = try await reducer.reduce(.snapshotReceived(
                TranscriptSnapshot(conversationID: convA, revision: ConversationRevision(rawValue: 1),
                    canonicalMessages: [
                        CanonicalMessage(canonicalMessageID: CanonicalMessageID("stale")!,
                            clientMessageID: nil, jobID: nil,
                            sequence: CanonicalSequence(rawValue: 1),
                            messageRevision: MessageRevision(rawValue: 1),
                            kind: .assistant, displayContent: "Stale!", deleted: false)
                    ]),
                NavigationEpoch(rawValue: 1)))
            #expect(Bool(false), "Stale epoch should have thrown")
        } catch TranscriptReducer.ReducerError.staleNavigationEpoch {
            // Expected
        }

        // Verify stale content did not leak
        let projection = await reducer.projection(for: convA)
        #expect(projection.orderedVisibleRows.isEmpty)
        #expect(projection.navigationEpoch == NavigationEpoch(rawValue: 2))
    }

    @Test("events with wrong conversation ID are rejected")
    func wrongConversationIDRejected() async throws {
        let reducer = TranscriptReducer()

        let convA = CanonicalConversationID("conv-A")!
        let convB = CanonicalConversationID("conv-B")!

        // Activate A at epoch 1
        _ = try await reducer.reduce(.activateConversation(convA, NavigationEpoch(rawValue: 1)))

        // Submit event for B — must be rejected
        do {
            _ = try await reducer.reduce(.snapshotReceived(
                TranscriptSnapshot(conversationID: convB, revision: .zero),
                NavigationEpoch(rawValue: 1)))
            #expect(Bool(false), "Mismatched conversation should have thrown")
        } catch TranscriptReducer.ReducerError.conversationMismatch {
            // Expected
        }
    }
}

// MARK: - Test 6: Queueing — two consecutive sends during streaming both produce user rows

@Suite("Queueing: concurrent sends preserve ordering")
struct QueueingTests {
    @Test("two consecutive sends during streaming both produce user rows in canonical order")
    func twoConcurrentSendsProduceOrderedRows() async throws {
        let reducer = TranscriptReducer()
        let scope = ConversationTaskScope(reducer: reducer)

        let conv = CanonicalConversationID("conv-1")!
        let epoch = NavigationEpoch(rawValue: 1)

        // Activate conversation
        _ = try await reducer.reduce(.activateConversation(conv, epoch))

        // Bind an assistant job (simulating an active stream)
        let assistantJobID = JobID("job-assistant")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conv, jobID: assistantJobID,
                canonicalMessageID: nil, renderID: TranscriptRenderID(),
                displayContent: "Thinking...", retryGeneration: nil), epoch))

        // Send 1: user submits while assistant is streaming
        let clientID1 = ClientMessageID("client-send-1")!
        let send1Row = OptimisticUserSubmission(conversationID: conv, clientMessageID: clientID1,
            displayContent: "First question")
        _ = try await reducer.reduce(.optimisticUserSubmitted(send1Row, epoch))

        // Send 2: user sends again while still streaming
        let clientID2 = ClientMessageID("client-send-2")!
        let send2Row = OptimisticUserSubmission(conversationID: conv, clientMessageID: clientID2,
            displayContent: "Second question")
        _ = try await reducer.reduce(.optimisticUserSubmitted(send2Row, epoch))

        // Verify both user rows are present in order
        let projection = await scope.projection(for: conv)
        #expect(projection.activeConversationID == conv)

        let userRows = projection.orderedVisibleRows.filter { $0.kind == .user }
        #expect(userRows.count == 2, "Both sends must produce user rows")

        // First send must appear before second (by localOrdinal)
        #expect(userRows[0].clientMessageID == clientID1)
        #expect(userRows[0].displayContent == "First question")
        #expect(userRows[0].lifecycle == .optimistic)

        #expect(userRows[1].clientMessageID == clientID2)
        #expect(userRows[1].displayContent == "Second question")
        #expect(userRows[1].lifecycle == .optimistic)

        // The assistant stream row must still be present
        let assistantRows = projection.orderedVisibleRows.filter { $0.kind == .assistant }
        #expect(assistantRows.count == 1)

        // Simulate server accepting both sends (acceptance)
        _ = try await reducer.reduce(.userSubmissionAccepted(
            UserSubmissionAcceptance(conversationID: conv, clientMessageID: clientID1,
                canonicalMessageID: CanonicalMessageID("can-send-1")!,
                sequence: CanonicalSequence(rawValue: 2),
                revision: MessageRevision(rawValue: 1),
                conversationRevision: ConversationRevision(rawValue: 2),
                displayContent: "First question"), epoch))

        _ = try await reducer.reduce(.userSubmissionAccepted(
            UserSubmissionAcceptance(conversationID: conv, clientMessageID: clientID2,
                canonicalMessageID: CanonicalMessageID("can-send-2")!,
                sequence: CanonicalSequence(rawValue: 3),
                revision: MessageRevision(rawValue: 1),
                conversationRevision: ConversationRevision(rawValue: 3),
                displayContent: "Second question"), epoch))

        // After both acceptances, both rows must still be present and ordered
        let finalProjection = await scope.projection(for: conv)
        let finalUserRows = finalProjection.orderedVisibleRows.filter { $0.kind == .user }
        #expect(finalUserRows.count == 2)
        #expect(finalUserRows[0].lifecycle == .accepted)
        #expect(finalUserRows[0].canonicalMessageID == CanonicalMessageID("can-send-1"))
        #expect(finalUserRows[1].lifecycle == .accepted)
        #expect(finalUserRows[1].canonicalMessageID == CanonicalMessageID("can-send-2"))
    }
}
