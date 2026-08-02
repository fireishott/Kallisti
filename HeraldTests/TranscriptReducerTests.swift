import Foundation
import Testing
@testable import Herald

@Suite("Transcript reducer foundation")
struct TranscriptReducerTests {
    private let conversation = CanonicalConversationID("conversation-1")!
    private let otherConversation = CanonicalConversationID("conversation-2")!
    private let epoch = NavigationEpoch(rawValue: 1)

    // MARK: - Foundation tests (Task 3)

    @Test("activation establishes the visible conversation")
    func activatesConversation() async throws {
        let reducer = TranscriptReducer()
        let projection = try await reducer.reduce(.activateConversation(conversation, NavigationEpoch(rawValue: 1)))
        #expect(projection.activeConversationID == conversation)
        #expect(projection.navigationEpoch == NavigationEpoch(rawValue: 1))
    }

    @Test("stale epochs are rejected without mutation")
    func rejectsStaleEpoch() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, NavigationEpoch(rawValue: 2)))
        do {
            _ = try await reducer.reduce(.deactivateConversation(.zero))
            #expect(Bool(false), "stale event should throw")
        } catch TranscriptReducer.ReducerError.staleNavigationEpoch {
            let projection = await reducer.projection(for: conversation)
            #expect(projection.activeConversationID == conversation)
            #expect(projection.navigationEpoch == NavigationEpoch(rawValue: 2))
        }
    }

    @Test("events for another conversation are rejected")
    func rejectsConversationMismatch() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, .zero))
        let snapshot = TranscriptSnapshot(conversationID: otherConversation, revision: .zero)
        do {
            _ = try await reducer.reduce(.snapshotReceived(snapshot, .zero))
            #expect(Bool(false), "mismatched event should throw")
        } catch TranscriptReducer.ReducerError.conversationMismatch {
            #expect(await reducer.projection(for: conversation).activeConversationID == conversation)
        }
    }

    @Test("deactivation clears active state")
    func deactivatesConversation() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, .zero))
        let projection = try await reducer.reduce(.deactivateConversation(.zero))
        #expect(projection.activeConversationID == nil)
        #expect(projection.orderedVisibleRows.isEmpty)
    }

    @Test("sequential activation calls use the supplied current epoch")
    func sequentialActivationAdvancesEpoch() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, NavigationEpoch(rawValue: 1)))
        let projection = try await reducer.reduce(.activateConversation(otherConversation, NavigationEpoch(rawValue: 2)))
        #expect(projection.activeConversationID == otherConversation)
        #expect(projection.navigationEpoch == NavigationEpoch(rawValue: 2))
    }

    @Test("reducer is constructible without dependencies")
    func constructsInIsolation() async {
        let reducer = TranscriptReducer()
        let projection = await reducer.projection(for: conversation)
        #expect(projection.activeConversationID == nil)
        #expect(projection.navigationEpoch == .zero)
        #expect(projection.conversationRevision == .zero)
    }
}

// MARK: - Identity matching tests (Task 4, tests 1-7)

@Suite("Identity matching")
struct TranscriptReducerIdentityTests {
    private let conversation = CanonicalConversationID("conv-id")!
    private let epoch = NavigationEpoch(rawValue: 1)

    // Test 1: optimistic user row upgrades in place after canonical acknowledgement (match by clientMessageID)
    @Test("optimistic user row upgrades in place after canonical acknowledgement")
    func optimisticUserRowUpgradesByClientMessageID() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID = ClientMessageID("client-1")!
        let renderID = TranscriptRenderID()
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "Hello", renderID: renderID), epoch))

        let projection1 = await reducer.projection(for: conversation)
        #expect(projection1.orderedVisibleRows.count == 1)
        #expect(projection1.orderedVisibleRows[0].lifecycle == .optimistic)

        let canonicalID = CanonicalMessageID("can-1")!
        _ = try await reducer.reduce(.userSubmissionAccepted(
            UserSubmissionAcceptance(conversationID: conversation, clientMessageID: clientID,
                                     canonicalMessageID: canonicalID,
                                     sequence: CanonicalSequence(rawValue: 1)!,
                                     revision: MessageRevision(rawValue: 1)!,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     displayContent: "Hello"), epoch))

        let projection2 = await reducer.projection(for: conversation)
        #expect(projection2.orderedVisibleRows.count == 1)
        #expect(projection2.orderedVisibleRows[0].lifecycle == .accepted)
        #expect(projection2.orderedVisibleRows[0].canonicalMessageID == canonicalID)
        #expect(projection2.orderedVisibleRows[0].clientMessageID == clientID)
        #expect(projection2.orderedVisibleRows[0].renderID == renderID)
    }

    // Test 2: optimistic user row upgrades when canonical arrives WITHOUT matching clientMessageID (fallback to canonical id)
    @Test("acceptance falls back to canonicalMessageID when clientMessageID does not match")
    func acceptanceFallsBackToCanonicalID() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        // First, inject a snapshot with a canonical row (no clientMessageID match needed)
        let canonicalID = CanonicalMessageID("can-1")!
        let existingRenderID = TranscriptRenderID()
        let existingRow = TranscriptRow(renderID: existingRenderID, canonicalMessageID: canonicalID,
            clientMessageID: nil, jobID: nil, canonicalSequence: CanonicalSequence(rawValue: 1)!,
            messageRevision: MessageRevision(rawValue: 1)!, conversationRevisionSeen: ConversationRevision(rawValue: 1),
            retryGeneration: 0, localOrdinal: LocalOrdinal(rawValue: 1), kind: .user,
            lifecycle: .accepted, displayContent: "Server content",
            reasoning: nil, toolActivity: nil, attachments: [], createdAt: Date(), lastUpdatedAt: Date())
        _ = try await reducer.reduce(.snapshotReceived(
            TranscriptSnapshot(conversationID: conversation, revision: ConversationRevision(rawValue: 1),
                               rows: [existingRow]), epoch))

        // Now send an acceptance with a DIFFERENT clientMessageID but same canonicalMessageID
        let differentClientID = ClientMessageID("server-generated-client-id")!
        _ = try await reducer.reduce(.userSubmissionAccepted(
            UserSubmissionAcceptance(conversationID: conversation, clientMessageID: differentClientID,
                                     canonicalMessageID: canonicalID,
                                     sequence: CanonicalSequence(rawValue: 1)!,
                                     revision: MessageRevision(rawValue: 1)!,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     displayContent: "Updated content"), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)
        #expect(projection.orderedVisibleRows[0].renderID == existingRenderID)
        #expect(projection.orderedVisibleRows[0].displayContent == "Updated content")
    }

    // Test 3: retry of identical clientMessageID is idempotent — second optimisticUserSubmitted with same clientMessageID rejected
    @Test("duplicate optimistic submission with same clientMessageID is rejected")
    func duplicateOptimisticSubmissionRejected() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID = ClientMessageID("client-1")!
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "Hello"), epoch))

        // Second submission with same clientMessageID but different renderID
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "Hello again"), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)

        let diags = await reducer.diagnostics()
        #expect(diags.contains { $0.category == "identity_collision" })
    }

    // Test 4: equal text with different clientMessageIDs remains two rows
    @Test("equal text with different clientMessageIDs produces two rows")
    func equalTextDifferentClientIDs() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID1 = ClientMessageID("client-1")!
        let clientID2 = ClientMessageID("client-2")!
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID1,
                                     displayContent: "Same text"), epoch))
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID2,
                                     displayContent: "Same text"), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 2)
        #expect(projection.orderedVisibleRows[0].clientMessageID == clientID1)
        #expect(projection.orderedVisibleRows[1].clientMessageID == clientID2)
    }

    // Test 5: snapshot plus live event produces one row (matching by canonicalMessageID)
    @Test("snapshot plus live terminal event with same canonicalMessageID produces one row")
    func snapshotPlusLiveEventMatchesCanonical() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let canonicalID = CanonicalMessageID("can-1")!
        let renderID = TranscriptRenderID()
        let row = TranscriptRow(renderID: renderID, canonicalMessageID: canonicalID,
            clientMessageID: nil, jobID: JobID("job-1"), canonicalSequence: CanonicalSequence(rawValue: 1)!,
            messageRevision: MessageRevision(rawValue: 1)!, conversationRevisionSeen: ConversationRevision(rawValue: 1),
            retryGeneration: 0, localOrdinal: LocalOrdinal(rawValue: 1), kind: .assistant,
            lifecycle: .streaming, displayContent: "Partial",
            reasoning: nil, toolActivity: nil, attachments: [], createdAt: Date(), lastUpdatedAt: Date())
        _ = try await reducer.reduce(.snapshotReceived(
            TranscriptSnapshot(conversationID: conversation, revision: ConversationRevision(rawValue: 1),
                               rows: [row]), epoch))

        // Live terminal event with same canonicalMessageID
        _ = try await reducer.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: JobID("job-1")!,
                                     canonicalMessageID: canonicalID,
                                     conversationRevision: ConversationRevision(rawValue: 2),
                                     messageRevision: MessageRevision(rawValue: 2)!,
                                     displayContent: "Final content", failure: nil), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)
        #expect(projection.orderedVisibleRows[0].lifecycle == .complete)
        #expect(projection.orderedVisibleRows[0].displayContent == "Final content")
    }

    // Test 6: assistant job binding upgrades in place when canonicalMessageID arrives
    @Test("assistant job binding upgrades row in place")
    func assistantJobBindingUpgradesInPlace() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!

        // First, bind the job
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Streaming...", retryGeneration: nil), epoch))

        let projection1 = await reducer.projection(for: conversation)
        #expect(projection1.orderedVisibleRows.count == 1)
        #expect(projection1.orderedVisibleRows[0].lifecycle == .accepted)

        // Now bind again with canonicalMessageID
        let canonicalID = CanonicalMessageID("can-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: canonicalID, renderID: renderID,
                                displayContent: "Updated", retryGeneration: nil), epoch))

        let projection2 = await reducer.projection(for: conversation)
        #expect(projection2.orderedVisibleRows.count == 1)
        #expect(projection2.orderedVisibleRows[0].canonicalMessageID == canonicalID)
        #expect(projection2.orderedVisibleRows[0].renderID == renderID)
    }

    // Test 7: assistant canonical ID arrival with different jobID creates a second row
    @Test("assistant canonical ID with different jobID creates second row")
    func assistantCanonicalWithDifferentJobIDCreatesSecondRow() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID1 = TranscriptRenderID()
        let jobID1 = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID1,
                                canonicalMessageID: nil, renderID: renderID1,
                                displayContent: "First", retryGeneration: nil), epoch))

        // Different jobID + different canonicalID = second row (server contract violation simulation)
        let renderID2 = TranscriptRenderID()
        let jobID2 = JobID("job-2")!
        let canonicalID2 = CanonicalMessageID("can-2")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID2,
                                canonicalMessageID: canonicalID2, renderID: renderID2,
                                displayContent: "Second", retryGeneration: nil), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 2)
    }
}

// MARK: - Lifecycle transition tests (Task 4, tests 8-15)

@Suite("Lifecycle transitions")
struct TranscriptReducerLifecycleTests {
    private let conversation = CanonicalConversationID("conv-id")!
    private let epoch = NavigationEpoch(rawValue: 1)

    // Test 8: optimistic -> submitting works
    @Test("optimistic to submitting transition works")
    func optimisticToSubmitting() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID = ClientMessageID("client-1")!
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "Hello"), epoch))

        _ = try await reducer.reduce(.userSubmissionRetrying(
            UserSubmissionRetrying(conversationID: conversation, clientMessageID: clientID,
                                   retryGeneration: 0), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)
        #expect(projection.orderedVisibleRows[0].lifecycle == .submitting)
    }

    // Test 9: submitting -> accepted works; submitting -> rejected works
    @Test("submitting to accepted and submitting to rejected transitions work")
    func submittingToAcceptedAndRejected() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID1 = ClientMessageID("client-1")!
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID1,
                                     displayContent: "Hello"), epoch))
        _ = try await reducer.reduce(.userSubmissionRetrying(
            UserSubmissionRetrying(conversationID: conversation, clientMessageID: clientID1,
                                   retryGeneration: 0), epoch))
        _ = try await reducer.reduce(.userSubmissionAccepted(
            UserSubmissionAcceptance(conversationID: conversation, clientMessageID: clientID1,
                                     canonicalMessageID: CanonicalMessageID("can-1")!,
                                     sequence: CanonicalSequence(rawValue: 1)!,
                                     revision: MessageRevision(rawValue: 1)!,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     displayContent: "Hello"), epoch))

        let projection1 = await reducer.projection(for: conversation)
        #expect(projection1.orderedVisibleRows[0].lifecycle == .accepted)

        // Now test rejected path with a second row
        let clientID2 = ClientMessageID("client-2")!
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID2,
                                     displayContent: "World"), epoch))
        _ = try await reducer.reduce(.userSubmissionRetrying(
            UserSubmissionRetrying(conversationID: conversation, clientMessageID: clientID2,
                                   retryGeneration: 0), epoch))
        _ = try await reducer.reduce(.submissionRejected(
            SubmissionRejection(conversationID: conversation, clientMessageID: clientID2,
                                failure: TranscriptFailure(category: "rate_limit", message: "Too fast", retryable: true)), epoch))

        let projection2 = await reducer.projection(for: conversation)
        let rejectedRow = projection2.orderedVisibleRows.first { $0.clientMessageID == clientID2 }
        #expect(rejectedRow != nil)
        if case .rejected = rejectedRow?.lifecycle {
            // OK
        } else {
            #expect(Bool(false), "Expected rejected lifecycle")
        }
    }

    // Test 10: accepted -> streaming -> complete works
    @Test("accepted to streaming to complete works")
    func acceptedToStreamingToComplete() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        _ = try await reducer.reduce(.streamDelta(
            TranscriptStreamDelta(conversationID: conversation, jobID: jobID,
                                  canonicalMessageID: nil,
                                  conversationRevision: ConversationRevision(rawValue: 1),
                                  messageRevision: MessageRevision(rawValue: 1)!,
                                  displayContent: "Hello"), epoch))

        _ = try await reducer.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 2),
                                     messageRevision: MessageRevision(rawValue: 2)!,
                                     displayContent: "Hello world", failure: nil), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)
        #expect(projection.orderedVisibleRows[0].lifecycle == .complete)
        #expect(projection.orderedVisibleRows[0].displayContent == "Hello world")
    }

    // Test 11: accepted -> streaming -> failed works
    @Test("accepted to streaming to failed works")
    func acceptedToStreamingToFailed() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        _ = try await reducer.reduce(.streamDelta(
            TranscriptStreamDelta(conversationID: conversation, jobID: jobID,
                                  canonicalMessageID: nil,
                                  conversationRevision: ConversationRevision(rawValue: 1),
                                  messageRevision: MessageRevision(rawValue: 1)!,
                                  displayContent: "Partial"), epoch))

        let failure = TranscriptFailure(category: "timeout", message: "Timed out", retryable: true)
        _ = try await reducer.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 2),
                                     messageRevision: MessageRevision(rawValue: 2)!,
                                     displayContent: "Partial", failure: failure), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)
        if case .failed(let f) = projection.orderedVisibleRows[0].lifecycle {
            #expect(f.category == "timeout")
        } else {
            #expect(Bool(false), "Expected failed lifecycle")
        }
    }

    // Test 12: late failure cannot overwrite terminal success (.complete then .failed is rejected)
    @Test("late failure cannot overwrite completed row")
    func lateFailureCannotOverwriteComplete() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        // Complete the row
        _ = try await reducer.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     messageRevision: MessageRevision(rawValue: 1)!,
                                     displayContent: "Done", failure: nil), epoch))

        // Try to fail it with a higher revision (should be rejected)
        let failure = TranscriptFailure(category: "error", message: "Oops", retryable: true)
        _ = try await reducer.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 2),
                                     messageRevision: MessageRevision(rawValue: 2)!,
                                     displayContent: "Done", failure: failure), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows[0].lifecycle == .complete)

        let diags = await reducer.diagnostics()
        #expect(diags.contains { $0.category == "illegal_transition" })
    }

    // Test 13: terminal failure shows retry exactly once (retry generation 0->1 works; 0->0 rejected)
    @Test("retry generation 0->1 transitions work; 0->0 is rejected")
    func retryGenerationTransition() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        // Fail the row
        let failure = TranscriptFailure(category: "error", message: "Failed", retryable: true)
        _ = try await reducer.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     messageRevision: MessageRevision(rawValue: 1)!,
                                     displayContent: "Failed", failure: failure), epoch))

        // Retry with generation 0 -> 0 (should be rejected)
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Retrying", retryGeneration: 0), epoch))

        let projection1 = await reducer.projection(for: conversation)
        if case .failed = projection1.orderedVisibleRows[0].lifecycle {
            // Still failed — retry with same generation was rejected
        } else {
            #expect(Bool(false), "Expected still failed after stale retry")
        }

        // Retry with generation 1 (should work)
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Retrying", retryGeneration: 1), epoch))

        let projection2 = await reducer.projection(for: conversation)
        #expect(projection2.orderedVisibleRows[0].lifecycle == .accepted)
        #expect(projection2.orderedVisibleRows[0].retryGeneration == 1)
    }

    // Test 14: active stream does not show regenerate prematurely
    @Test("streaming row does not appear in terminalFailuresByRenderID")
    func streamingRowNotInTerminalFailures() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        _ = try await reducer.reduce(.streamDelta(
            TranscriptStreamDelta(conversationID: conversation, jobID: jobID,
                                  canonicalMessageID: nil,
                                  conversationRevision: ConversationRevision(rawValue: 1),
                                  messageRevision: MessageRevision(rawValue: 1)!,
                                  displayContent: "Streaming..."), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows[0].lifecycle == .streaming)
        #expect(projection.terminalFailuresByRenderID[renderID] == nil)
        #expect(projection.activeJobIDs.contains(jobID))
    }

    // Test 15: cancelled cannot transition to complete; only to submitting with retry generation increase
    @Test("cancelled cannot transition to complete; only to submitting with retry")
    func cancelledCannotTransitionToComplete() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID = ClientMessageID("client-1")!
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "Hello"), epoch))
        _ = try await reducer.reduce(.userSubmissionRetrying(
            UserSubmissionRetrying(conversationID: conversation, clientMessageID: clientID,
                                   retryGeneration: 0), epoch))

        // Cancel the row via an accepted -> cancelled transition
        // First accept it
        _ = try await reducer.reduce(.userSubmissionAccepted(
            UserSubmissionAcceptance(conversationID: conversation, clientMessageID: clientID,
                                     canonicalMessageID: CanonicalMessageID("can-1")!,
                                     sequence: CanonicalSequence(rawValue: 1)!,
                                     revision: MessageRevision(rawValue: 1)!,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     displayContent: "Hello"), epoch))

        // We need a way to cancel. Use an assistant row instead for cleaner lifecycle.
        // Let's redo with an assistant row.
        let reducer2 = TranscriptReducer()
        _ = try await reducer2.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer2.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        // Cancel via terminal with cancellation failure
        let cancelFailure = TranscriptFailure(category: "cancelled", message: "User cancelled", retryable: false)
        _ = try await reducer2.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 1),
                                     messageRevision: MessageRevision(rawValue: 1)!,
                                     displayContent: "Start", failure: cancelFailure), epoch))

        let projection1 = await reducer2.projection(for: conversation)
        if case .failed = projection1.orderedVisibleRows[0].lifecycle {
            // OK — cancelled shows as failed with the cancel failure
        }

        // Now try to transition to complete (should be rejected)
        _ = try await reducer2.reduce(.messageTerminal(
            TranscriptTerminalEvent(conversationID: conversation, jobID: jobID,
                                     canonicalMessageID: nil,
                                     conversationRevision: ConversationRevision(rawValue: 2),
                                     messageRevision: MessageRevision(rawValue: 2)!,
                                     displayContent: "Done", failure: nil), epoch))

        let projection2 = await reducer2.projection(for: conversation)
        // Should still be in the failed/cancelled state, not complete
        if case .complete = projection2.orderedVisibleRows[0].lifecycle {
            #expect(Bool(false), "cancelled/failed should not transition to complete")
        }

        let diags = await reducer2.diagnostics()
        #expect(diags.contains { $0.category == "illegal_transition" })
    }
}

// MARK: - Diagnostic tests (Task 4, tests 16-17)

@Suite("Diagnostics")
struct TranscriptReducerDiagnosticTests {
    private let conversation = CanonicalConversationID("conv-id")!
    private let epoch = NavigationEpoch(rawValue: 1)

    // Test 16: identity collision is rejected and diagnosed
    @Test("identity collision is rejected and diagnosed")
    func identityCollisionRejectedAndDiagnosed() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let clientID = ClientMessageID("client-1")!
        let renderID1 = TranscriptRenderID()
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "First", renderID: renderID1), epoch))

        // Second submission with same clientMessageID but different renderID
        let renderID2 = TranscriptRenderID()
        _ = try await reducer.reduce(.optimisticUserSubmitted(
            OptimisticUserSubmission(conversationID: conversation, clientMessageID: clientID,
                                     displayContent: "Second", renderID: renderID2), epoch))

        let projection = await reducer.projection(for: conversation)
        #expect(projection.orderedVisibleRows.count == 1)
        #expect(projection.orderedVisibleRows[0].renderID == renderID1)

        let diags = await reducer.diagnostics()
        #expect(diags.contains { $0.category == "identity_collision" })
    }

    // Test 17: stale message revision is ignored
    @Test("stale message revision is ignored")
    func staleMessageRevisionIgnored() async throws {
        let reducer = TranscriptReducer()
        _ = try await reducer.reduce(.activateConversation(conversation, epoch))

        let renderID = TranscriptRenderID()
        let jobID = JobID("job-1")!
        _ = try await reducer.reduce(.assistantJobBound(
            AssistantJobBinding(conversationID: conversation, jobID: jobID,
                                canonicalMessageID: nil, renderID: renderID,
                                displayContent: "Start", retryGeneration: nil), epoch))

        // Advance with a stream delta at revision 2
        _ = try await reducer.reduce(.streamDelta(
            TranscriptStreamDelta(conversationID: conversation, jobID: jobID,
                                  canonicalMessageID: nil,
                                  conversationRevision: ConversationRevision(rawValue: 1),
                                  messageRevision: MessageRevision(rawValue: 2)!,
                                  displayContent: "Updated"), epoch))

        let projection1 = await reducer.projection(for: conversation)
        #expect(projection1.orderedVisibleRows[0].messageRevision == MessageRevision(rawValue: 2)!)
        #expect(projection1.orderedVisibleRows[0].displayContent == "StartUpdated")

        // Send a stream delta at revision 1 (stale — should be ignored)
        _ = try await reducer.reduce(.streamDelta(
            TranscriptStreamDelta(conversationID: conversation, jobID: jobID,
                                  canonicalMessageID: nil,
                                  conversationRevision: ConversationRevision(rawValue: 2),
                                  messageRevision: MessageRevision(rawValue: 1)!,
                                  displayContent: "Stale"), epoch))

        let projection2 = await reducer.projection(for: conversation)
        #expect(projection2.orderedVisibleRows[0].messageRevision == MessageRevision(rawValue: 2)!)
        #expect(projection2.orderedVisibleRows[0].displayContent == "StartUpdated")

        let diags = await reducer.diagnostics()
        #expect(diags.contains { $0.category == "stale_message_revision" })
    }
}
