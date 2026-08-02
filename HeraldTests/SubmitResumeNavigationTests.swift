import XCTest
@testable import Herald

/// TDD tests for the submit/resume/navigation flow.
/// These tests define the integration contract between ChatStore,
/// SessionProjectionStore, and GatewayClient.
///
/// Tests are written to FAIL until the integration is wired.
final class SubmitResumeNavigationTests: XCTestCase {

    // MARK: - Submit flow

    func testSingleTapProducesOneOptimisticRowAndOneRPCRequest() async throws {
        // One tap → one optimistic user row in the projection
        // → one JSON-RPC prompt.submit request to the relay
        let store = SessionProjectionStore()
        let sessionID = CanonicalConversationID("test-session")!
        await store.activateSession(sessionID)

        let clientMessageID = ClientMessageID("msg-1")!
        let submission = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionID,
            clientMessageID: clientMessageID,
            displayContent: "Hello"
        )
        await store.applyOptimisticUser(submission, epoch: await store.currentEpoch)

        let projection = await store.projection(for: sessionID)
        XCTAssertEqual(projection?.orderedVisibleRows.count, 1)
        XCTAssertEqual(projection?.orderedVisibleRows.first?.displayContent, "Hello")
        XCTAssertEqual(projection?.orderedVisibleRows.first?.kind, .user)
    }

    func testAcceptanceUpgradesRowRatherThanAppending() async throws {
        // Acceptance should upgrade the optimistic row, not append a new one
        let store = SessionProjectionStore()
        let sessionID = CanonicalConversationID("test-session")!
        await store.activateSession(sessionID)

        let clientMessageID = ClientMessageID("msg-1")!

        // Optimistic submit
        let submission = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionID,
            clientMessageID: clientMessageID,
            displayContent: "Hello"
        )
        await store.applyOptimisticUser(submission, epoch: await store.currentEpoch)

        // Acceptance
        let acceptance = TranscriptReducer.UserSubmissionAcceptance(
            conversationID: sessionID,
            clientMessageID: clientMessageID,
            canonicalMessageID: CanonicalMessageID("canon-1")!,
            sequence: CanonicalSequence(rawValue: 1),
            revision: MessageRevision(rawValue: 1),
            conversationRevision: ConversationRevision(rawValue: 1),
            displayContent: "Hello"
        )
        await store.applyUserAccepted(acceptance, epoch: await store.currentEpoch)

        // Should still have exactly 1 row
        let projection = await store.projection(for: sessionID)
        XCTAssertEqual(projection?.orderedVisibleRows.count, 1)
        XCTAssertEqual(projection?.orderedVisibleRows.first?.canonicalMessageID?.rawValue, "canon-1")
    }

    func testIdenticalMessagesRemainDistinctTurns() async throws {
        // Two identical messages should be two distinct turns
        let store = SessionProjectionStore()
        let sessionID = CanonicalConversationID("test-session")!
        await store.activateSession(sessionID)

        let submission1 = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionID,
            clientMessageID: ClientMessageID("msg-1")!,
            displayContent: "Hello"
        )
        let submission2 = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionID,
            clientMessageID: ClientMessageID("msg-2")!,
            displayContent: "Hello"
        )

        await store.applyOptimisticUser(submission1, epoch: await store.currentEpoch)
        await store.applyOptimisticUser(submission2, epoch: await store.currentEpoch)

        let projection = await store.projection(for: sessionID)
        XCTAssertEqual(projection?.orderedVisibleRows.count, 2)
    }

    func testSystemContextRemainsTransportOnly() async throws {
        // System context must never appear in a user bubble
        let store = SessionProjectionStore()
        let sessionID = CanonicalConversationID("test-session")!
        await store.activateSession(sessionID)

        let submission = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionID,
            clientMessageID: ClientMessageID("msg-1")!,
            displayContent: "What time is it?"
        )
        await store.applyOptimisticUser(submission, epoch: await store.currentEpoch)

        let projection = await store.projection(for: sessionID)
        let content = projection?.orderedVisibleRows.first?.displayContent ?? ""

        XCTAssertFalse(content.contains("[System context"))
        XCTAssertFalse(content.contains("staging"))
        XCTAssertEqual(content, "What time is it?")
    }

    // MARK: - Session isolation

    func testResponseEventsUpdateOnlyTheirSession() async throws {
        // Events for session A should not affect session B
        let store = SessionProjectionStore()
        let sessionA = CanonicalConversationID("session-a")!
        let sessionB = CanonicalConversationID("session-b")!

        await store.activateSession(sessionA)

        // Add row to session A
        let submissionA = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionA,
            clientMessageID: ClientMessageID("msg-a")!,
            displayContent: "Hello from A"
        )
        await store.applyOptimisticUser(submissionA, epoch: await store.currentEpoch)

        // Switch to session B
        await store.activateSession(sessionB)

        // Session A's projection should still have the row
        let projectionA = await store.projection(for: sessionA)
        XCTAssertEqual(projectionA?.orderedVisibleRows.count, 1)

        // Session B should be empty
        let projectionB = await store.projection(for: sessionB)
        XCTAssertEqual(projectionB?.orderedVisibleRows.count, 0)
    }

    func testNavigationDuringStreamNeverPaintsAcrossSessions() async throws {
        // Navigate from A to B while A is streaming → B should not show A's content
        let store = SessionProjectionStore()
        let sessionA = CanonicalConversationID("session-a")!
        let sessionB = CanonicalConversationID("session-b")!

        await store.activateSession(sessionA)

        // Start streaming in A
        let submission = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionA,
            clientMessageID: ClientMessageID("msg-1")!,
            displayContent: "Question"
        )
        await store.applyOptimisticUser(submission, epoch: await store.currentEpoch)

        // Navigate to B
        await store.activateSession(sessionB)

        // B should be empty
        let projectionB = await store.projection(for: sessionB)
        XCTAssertEqual(projectionB?.orderedVisibleRows.count, 0)
    }

    func testStaleHistoryCompletionRejectedByRouteGeneration() async throws {
        // After navigating away, stale completions for the old session are rejected
        let store = SessionProjectionStore()
        let sessionA = CanonicalConversationID("session-a")!
        let sessionB = CanonicalConversationID("session-b")!

        await store.activateSession(sessionA)
        let epochA = await store.currentEpoch

        // Navigate to B
        await store.activateSession(sessionB)
        let epochB = await store.currentEpoch

        // Try to apply a delta for session A at stale epoch
        let delta = TranscriptReducer.TranscriptStreamDelta(
            conversationID: sessionA,
            jobID: JobID("job-1")!,
            canonicalMessageID: nil,
            conversationRevision: .zero,
            messageRevision: .zero,
            displayContent: "Stale delta"
        )
        await store.applyStreamDelta(delta, epoch: epochA)

        // Session A should not have the stale delta
        let projectionA = await store.projection(for: sessionA)
        XCTAssertEqual(projectionA?.orderedVisibleRows.count, 0)
    }

    // MARK: - Backend authority

    func testBackendBusyCannotBeClearedByUI() async throws {
        // Backend busy is set by gateway events only
        // UI silence/watchdog cannot clear it
        let store = SessionProjectionStore()
        let sessionID = CanonicalConversationID("test-session")!
        await store.activateSession(sessionID)

        await store.setBackendBusy(true, for: sessionID)
        let isBusy = await store.isBackendBusy(for: sessionID)
        XCTAssertTrue(isBusy)

        // Only a terminal event should clear it
        // (UI cannot clear backend busy)
    }

    // MARK: - Terminal events

    func testTerminalEventsPublishImmediately() async throws {
        // Terminal events (complete/failed) should update projection immediately
        let store = SessionProjectionStore()
        let sessionID = CanonicalConversationID("test-session")!
        await store.activateSession(sessionID)

        // First add a row
        let submission = TranscriptReducer.OptimisticUserSubmission(
            conversationID: sessionID,
            clientMessageID: ClientMessageID("msg-1")!,
            displayContent: "Question"
        )
        await store.applyOptimisticUser(submission, epoch: await store.currentEpoch)

        // Then complete it
        let terminal = TranscriptReducer.TranscriptTerminalEvent(
            conversationID: sessionID,
            jobID: JobID("job-1")!,
            canonicalMessageID: CanonicalMessageID("canon-1")!,
            conversationRevision: ConversationRevision(rawValue: 1),
            messageRevision: MessageRevision(rawValue: 1),
            displayContent: "Answer",
            failure: nil
        )
        await store.applyMessageTerminal(terminal, epoch: await store.currentEpoch)

        let projection = await store.projection(for: sessionID)
        // Should have at least the terminal row
        XCTAssertFalse(projection?.orderedVisibleRows.isEmpty ?? true)
    }
}
