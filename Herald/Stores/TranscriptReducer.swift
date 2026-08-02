import Foundation

actor TranscriptReducer {
    struct State: Sendable {
        var activeConversationID: CanonicalConversationID?
        var navigationEpoch: NavigationEpoch
        var conversationRevision: ConversationRevision
        var rowsByRenderID: [TranscriptRenderID: TranscriptRow]
        var renderIDByCanonicalID: [CanonicalMessageID: TranscriptRenderID]
        var renderIDByClientID: [ClientMessageID: TranscriptRenderID]
        var renderIDByJobID: [JobID: TranscriptRenderID]
        var nextLocalOrdinal: UInt64
    }

    /// A canonical message as delivered by the server in a snapshot.
    struct CanonicalMessage: Sendable {
        let canonicalMessageID: CanonicalMessageID
        let clientMessageID: ClientMessageID?
        let jobID: JobID?
        let sequence: CanonicalSequence
        let messageRevision: MessageRevision
        let kind: TranscriptRowKind
        let displayContent: String
        let deleted: Bool
    }

    struct TranscriptSnapshot: Sendable {
        let conversationID: CanonicalConversationID
        let revision: ConversationRevision
        let rows: [TranscriptRow]
        /// Canonical messages from the server ledger for reconciliation.
        let canonicalMessages: [CanonicalMessage]
        init(conversationID: CanonicalConversationID, revision: ConversationRevision,
             rows: [TranscriptRow] = [], canonicalMessages: [CanonicalMessage] = []) {
            self.conversationID = conversationID
            self.revision = revision
            self.rows = rows
            self.canonicalMessages = canonicalMessages
        }
    }

    struct OptimisticUserSubmission: Sendable {
        let conversationID: CanonicalConversationID
        let clientMessageID: ClientMessageID
        let displayContent: String
        let renderID: TranscriptRenderID
        init(conversationID: CanonicalConversationID, clientMessageID: ClientMessageID,
             displayContent: String, renderID: TranscriptRenderID = TranscriptRenderID()) {
            self.conversationID = conversationID; self.clientMessageID = clientMessageID
            self.displayContent = displayContent; self.renderID = renderID
        }
    }

    struct UserSubmissionAcceptance: Sendable {
        let conversationID: CanonicalConversationID
        let clientMessageID: ClientMessageID
        let canonicalMessageID: CanonicalMessageID
        let sequence: CanonicalSequence
        let revision: MessageRevision
        let conversationRevision: ConversationRevision
        let displayContent: String
    }

    struct UserSubmissionRetrying: Sendable {
        let conversationID: CanonicalConversationID
        let clientMessageID: ClientMessageID
        let retryGeneration: Int
    }

    struct AssistantJobBinding: Sendable {
        let conversationID: CanonicalConversationID
        let jobID: JobID
        let canonicalMessageID: CanonicalMessageID?
        let renderID: TranscriptRenderID
        let displayContent: String
        let retryGeneration: Int?
    }

    struct TranscriptStreamDelta: Sendable {
        let conversationID: CanonicalConversationID
        let jobID: JobID
        let canonicalMessageID: CanonicalMessageID?
        let conversationRevision: ConversationRevision
        let messageRevision: MessageRevision
        let displayContent: String
    }

    struct TranscriptTerminalEvent: Sendable {
        let conversationID: CanonicalConversationID
        let jobID: JobID
        let canonicalMessageID: CanonicalMessageID?
        let conversationRevision: ConversationRevision
        let messageRevision: MessageRevision
        let displayContent: String
        let failure: TranscriptFailure?
    }

    struct SubmissionRejection: Sendable {
        let conversationID: CanonicalConversationID
        let clientMessageID: ClientMessageID
        let failure: TranscriptFailure
    }

    struct TranscriptDeletion: Sendable {
        let conversationID: CanonicalConversationID
        let renderID: TranscriptRenderID
    }

    struct TranscriptProjection: Sendable {
        let activeConversationID: CanonicalConversationID?
        let navigationEpoch: NavigationEpoch
        let conversationRevision: ConversationRevision
        let orderedVisibleRows: [TranscriptRow]
        let activeJobIDs: Set<JobID>
        let terminalFailuresByRenderID: [TranscriptRenderID: TranscriptFailure]
        let cacheableCanonicalState: Data?
    }

    enum Event: Sendable {
        case activateConversation(CanonicalConversationID, NavigationEpoch)
        case deactivateConversation(NavigationEpoch)
        case hydrateCache(TranscriptSnapshot, NavigationEpoch)
        case optimisticUserSubmitted(OptimisticUserSubmission, NavigationEpoch)
        case userSubmissionAccepted(UserSubmissionAcceptance, NavigationEpoch)
        case userSubmissionRetrying(UserSubmissionRetrying, NavigationEpoch)
        case assistantJobBound(AssistantJobBinding, NavigationEpoch)
        case streamDelta(TranscriptStreamDelta, NavigationEpoch)
        case snapshotReceived(TranscriptSnapshot, NavigationEpoch)
        case messageTerminal(TranscriptTerminalEvent, NavigationEpoch)
        case submissionRejected(SubmissionRejection, NavigationEpoch)
        case explicitDeletion(TranscriptDeletion, NavigationEpoch)
    }

    enum ReducerError: Error, Equatable {
        case staleNavigationEpoch(expected: NavigationEpoch, received: NavigationEpoch)
        case conversationMismatch(expected: CanonicalConversationID, received: CanonicalConversationID)
        case unknownRenderID(TranscriptRenderID)
    }

    private var state: State
    private var rejectionDiagnostics: [TranscriptDiagnostic] = []

    init() {
        self.state = State(activeConversationID: nil, navigationEpoch: .zero,
                           conversationRevision: .zero, rowsByRenderID: [:],
                           renderIDByCanonicalID: [:], renderIDByClientID: [:],
                           renderIDByJobID: [:], nextLocalOrdinal: 1)
    }

    // MARK: - Lifecycle transition table

    /// Encodes the exact transition table from plan §3E.
    /// Retry transitions (failed->accepted, rejected->submitting, cancelled->submitting)
    /// return true here but the caller MUST also verify retry generation.
    private nonisolated func canTransition(from: TranscriptRowLifecycle, to: TranscriptRowLifecycle) -> Bool {
        switch (from, to) {
        // User submission lifecycle
        case (.optimistic, .submitting): return true
        case (.optimistic, .accepted): return true
        case (.submitting, .accepted): return true
        case (.submitting, .rejected): return true
        case (.submitting, .failed): return true

        // Active lifecycle (idempotent accepted for job binding upgrades)
        case (.accepted, .accepted): return true
        case (.accepted, .streaming): return true
        case (.accepted, .complete): return true
        case (.accepted, .failed): return true
        case (.accepted, .cancelled): return true

        // Streaming lifecycle
        case (.streaming, .streaming): return true
        case (.streaming, .complete): return true
        case (.streaming, .failed): return true
        case (.streaming, .cancelled): return true

        // Idempotent terminal (newer metadata only)
        case (.complete, .complete): return true

        // Retry transitions (require retry generation check by caller)
        case (.failed, .accepted): return true
        case (.failed, .streaming): return true
        case (.rejected, .submitting): return true
        case (.cancelled, .submitting): return true

        // Everything else is illegal
        default: return false
        }
    }

    // MARK: - Main reducer

    func reduce(_ event: Event) throws -> TranscriptProjection {
        switch event {
        case let .activateConversation(id, epoch):
            // activateConversation is the epoch-setting event. It must accept any epoch
            // because the caller (ConversationTaskScope.navigateTo) increments the epoch
            // before calling this. The epoch check applies to all subsequent events.
            state.activeConversationID = id
            state.navigationEpoch = epoch
            state.conversationRevision = .zero
            clearRows()

        case let .deactivateConversation(epoch):
            try requireEpoch(epoch)
            state.activeConversationID = nil
            state.conversationRevision = .zero
            clearRows()

        case let .hydrateCache(snapshot, epoch), let .snapshotReceived(snapshot, epoch):
            try requireEpoch(epoch); try requireConversation(snapshot.conversationID)
            // Reject snapshot with conversation revision older than current authoritative revision
            guard snapshot.revision >= state.conversationRevision else {
                record("stale_conversation_revision",
                       "Snapshot revision \(snapshot.revision.rawValue) < reducer revision \(state.conversationRevision.rawValue)")
                break
            }
            reconcileSnapshot(snapshot)

        // MARK: User optimistic submission
        case let .optimisticUserSubmitted(submission, epoch):
            try requireEpoch(epoch); try requireConversation(submission.conversationID)
            // Collision check: reject if clientMessageID already bound to a different render row
            if let existingRenderID = state.renderIDByClientID[submission.clientMessageID],
               existingRenderID != submission.renderID {
                recordCollision(identity: submission.clientMessageID.rawValue, priorRenderID: existingRenderID,
                                eventKind: "optimisticUserSubmitted", reason: "clientMessageID already bound to a different render row")
                break
            }
            let now = Date()
            let row = TranscriptRow(renderID: submission.renderID, canonicalMessageID: nil,
                clientMessageID: submission.clientMessageID, jobID: nil, canonicalSequence: nil,
                messageRevision: .zero, conversationRevisionSeen: state.conversationRevision,
                retryGeneration: 0, localOrdinal: LocalOrdinal(rawValue: state.nextLocalOrdinal),
                kind: .user, lifecycle: .optimistic, displayContent: submission.displayContent,
                reasoning: nil, toolActivity: nil, attachments: [], createdAt: now, lastUpdatedAt: now)
            state.nextLocalOrdinal &+= 1
            insert(row)

        // MARK: User submission accepted
        case let .userSubmissionAccepted(acceptance, epoch):
            try requireEpoch(epoch); try requireConversation(acceptance.conversationID)
            // Identity matching: clientMessageID first, then canonicalMessageID
            var targetRow: TranscriptRow?
            var targetRenderID: TranscriptRenderID?

            if let renderID = state.renderIDByClientID[acceptance.clientMessageID],
               let row = state.rowsByRenderID[renderID] {
                targetRow = row
                targetRenderID = renderID
            } else if let renderID = state.renderIDByCanonicalID[acceptance.canonicalMessageID],
                      let row = state.rowsByRenderID[renderID] {
                targetRow = row
                targetRenderID = renderID
            }

            guard var row = targetRow, let renderID = targetRenderID else {
                // No existing row found — create a new render row
                let newRenderID = TranscriptRenderID()
                let now = Date()
                let newRow = TranscriptRow(renderID: newRenderID, canonicalMessageID: acceptance.canonicalMessageID,
                    clientMessageID: acceptance.clientMessageID, jobID: nil,
                    canonicalSequence: acceptance.sequence, messageRevision: acceptance.revision,
                    conversationRevisionSeen: acceptance.conversationRevision, retryGeneration: 0,
                    localOrdinal: LocalOrdinal(rawValue: state.nextLocalOrdinal), kind: .user,
                    lifecycle: .accepted, displayContent: acceptance.displayContent,
                    reasoning: nil, toolActivity: nil, attachments: [], createdAt: now, lastUpdatedAt: now)
                state.nextLocalOrdinal &+= 1
                insert(newRow)
                state.conversationRevision = max(state.conversationRevision, acceptance.conversationRevision)
                break
            }

            // Check for collision: if we found by clientMessageID, verify canonicalMessageID doesn't point elsewhere
            if let existingByCanonical = state.renderIDByCanonicalID[acceptance.canonicalMessageID],
               existingByCanonical != renderID {
                recordCollision(identity: acceptance.canonicalMessageID.rawValue, priorRenderID: existingByCanonical,
                                eventKind: "userSubmissionAccepted", reason: "canonicalMessageID already bound to a different render row")
                break
            }

            // Upgrade existing row in place — never append a second row
            guard canTransition(from: row.lifecycle, to: .accepted) else {
                record("illegal_transition", "Cannot transition from \(row.lifecycle) to accepted")
                break
            }
            let updatedRow = TranscriptRow(renderID: row.renderID,
                canonicalMessageID: acceptance.canonicalMessageID,
                clientMessageID: row.clientMessageID ?? acceptance.clientMessageID,
                jobID: row.jobID, canonicalSequence: acceptance.sequence,
                messageRevision: acceptance.revision,
                conversationRevisionSeen: acceptance.conversationRevision,
                retryGeneration: row.retryGeneration, localOrdinal: row.localOrdinal,
                kind: row.kind, lifecycle: .accepted,
                displayContent: acceptance.displayContent,
                reasoning: row.reasoning, toolActivity: row.toolActivity,
                attachments: row.attachments, createdAt: row.createdAt, lastUpdatedAt: Date())
            replace(row: updatedRow)
            state.conversationRevision = max(state.conversationRevision, acceptance.conversationRevision)

        // MARK: User submission retrying (optimistic->submitting, or rejected/cancelled->submitting)
        case let .userSubmissionRetrying(retrying, epoch):
            try requireEpoch(epoch); try requireConversation(retrying.conversationID)
            guard let renderID = state.renderIDByClientID[retrying.clientMessageID],
                  var row = state.rowsByRenderID[renderID] else {
                record("missing_client_identity", "Retry did not match an existing row by clientMessageID")
                break
            }
            guard canTransition(from: row.lifecycle, to: .submitting) else {
                record("illegal_transition", "Cannot transition from \(row.lifecycle) to submitting")
                break
            }
            // For retry from rejected/cancelled, verify retry generation increases
            switch row.lifecycle {
            case .rejected, .cancelled:
                guard retrying.retryGeneration > row.retryGeneration else {
                    record("stale_retry_generation", "Retry generation \(retrying.retryGeneration) not greater than current \(row.retryGeneration)")
                    break
                }
            default:
                break
            }
            row.lifecycle = .submitting
            row.retryGeneration = retrying.retryGeneration
            row.lastUpdatedAt = Date()
            replace(row: row)

        // MARK: Assistant job binding
        case let .assistantJobBound(binding, epoch):
            try requireEpoch(epoch); try requireConversation(binding.conversationID)
            // Identity matching: jobID first, then canonicalMessageID, then renderID
            var targetRow: TranscriptRow?
            var targetRenderID: TranscriptRenderID?

            if let renderID = state.renderIDByJobID[binding.jobID],
               let row = state.rowsByRenderID[renderID] {
                targetRow = row
                targetRenderID = renderID
            } else if let canonicalID = binding.canonicalMessageID,
                      let renderID = state.renderIDByCanonicalID[canonicalID],
                      let row = state.rowsByRenderID[renderID] {
                targetRow = row
                targetRenderID = renderID
            } else if let row = state.rowsByRenderID[binding.renderID] {
                targetRow = row
                targetRenderID = binding.renderID
            }

            if var row = targetRow, let renderID = targetRenderID {
                // Check for collision: if jobID is already bound to a different row
                if let existingByJob = state.renderIDByJobID[binding.jobID],
                   existingByJob != renderID {
                    recordCollision(identity: binding.jobID.rawValue, priorRenderID: existingByJob,
                                    eventKind: "assistantJobBound", reason: "jobID already bound to a different render row")
                    break
                }
                // Check retry generation for failed -> accepted transition
                if case .failed = row.lifecycle {
                    if let retryGen = binding.retryGeneration {
                        guard retryGen > row.retryGeneration else {
                            record("stale_retry_generation", "Retry generation \(retryGen) not greater than current \(row.retryGeneration)")
                            break
                        }
                        row.retryGeneration = retryGen
                    } else {
                        record("illegal_transition", "Cannot transition from failed to accepted without retry generation")
                        break
                    }
                }
                guard canTransition(from: row.lifecycle, to: .accepted) else {
                    record("illegal_transition", "Cannot transition from \(row.lifecycle) to accepted")
                    break
                }
                row.lifecycle = .accepted
                row.displayContent = binding.displayContent
                row.lastUpdatedAt = Date()
                row = TranscriptRow(renderID: row.renderID, canonicalMessageID: binding.canonicalMessageID ?? row.canonicalMessageID,
                    clientMessageID: row.clientMessageID, jobID: binding.jobID, canonicalSequence: row.canonicalSequence,
                    messageRevision: row.messageRevision, conversationRevisionSeen: row.conversationRevisionSeen,
                    retryGeneration: row.retryGeneration, localOrdinal: row.localOrdinal, kind: .assistant,
                    lifecycle: row.lifecycle, displayContent: row.displayContent, reasoning: row.reasoning,
                    toolActivity: row.toolActivity, attachments: row.attachments,
                    createdAt: row.createdAt, lastUpdatedAt: row.lastUpdatedAt)
                insert(row)
                state.renderIDByJobID[binding.jobID] = renderID
            } else {
                // Brand new job — create a new render row
                let now = Date()
                let newRow = TranscriptRow(renderID: binding.renderID, canonicalMessageID: binding.canonicalMessageID,
                    clientMessageID: nil, jobID: binding.jobID, canonicalSequence: nil,
                    messageRevision: .zero, conversationRevisionSeen: state.conversationRevision,
                    retryGeneration: 0, localOrdinal: LocalOrdinal(rawValue: state.nextLocalOrdinal),
                    kind: .assistant, lifecycle: .accepted, displayContent: binding.displayContent,
                    reasoning: nil, toolActivity: nil, attachments: [], createdAt: now, lastUpdatedAt: now)
                state.nextLocalOrdinal &+= 1
                insert(newRow)
            }

        // MARK: Stream delta
        case let .streamDelta(delta, epoch):
            try requireEpoch(epoch); try requireConversation(delta.conversationID)
            // Identity matching: canonicalMessageID first, then jobID
            var matchedRenderID: TranscriptRenderID?
            if let canonicalID = delta.canonicalMessageID,
               let renderID = state.renderIDByCanonicalID[canonicalID] {
                matchedRenderID = renderID
            } else if let renderID = state.renderIDByJobID[delta.jobID] {
                matchedRenderID = renderID
            }

            guard let renderID = matchedRenderID, var row = state.rowsByRenderID[renderID] else {
                record("unknown_stream_target", "Stream delta for unknown jobID \(delta.jobID.rawValue) and no canonical match")
                break
            }
            // Reject stale message revision
            guard delta.messageRevision >= row.messageRevision else {
                record("stale_message_revision", "Stream delta revision \(delta.messageRevision.rawValue) < row revision \(row.messageRevision.rawValue)")
                break
            }
            guard canTransition(from: row.lifecycle, to: .streaming) else {
                record("illegal_transition", "Cannot transition from \(row.lifecycle) to streaming")
                break
            }
            row.lifecycle = .streaming
            row.displayContent += delta.displayContent
            row.messageRevision = delta.messageRevision
            row.lastUpdatedAt = Date()
            replace(row: row)
            state.conversationRevision = max(state.conversationRevision, delta.conversationRevision)

        // MARK: Message terminal
        case let .messageTerminal(terminal, epoch):
            try requireEpoch(epoch); try requireConversation(terminal.conversationID)
            // Identity matching: canonicalMessageID first, then jobID
            var matchedRenderID: TranscriptRenderID?
            if let canonicalID = terminal.canonicalMessageID,
               let renderID = state.renderIDByCanonicalID[canonicalID] {
                matchedRenderID = renderID
            } else if let renderID = state.renderIDByJobID[terminal.jobID] {
                matchedRenderID = renderID
            }

            guard let renderID = matchedRenderID, var row = state.rowsByRenderID[renderID] else {
                record("unknown_terminal_target", "Terminal event for unknown jobID \(terminal.jobID.rawValue) and no canonical match")
                break
            }
            // Reject stale message revision
            guard terminal.messageRevision >= row.messageRevision else {
                record("stale_message_revision", "Terminal revision \(terminal.messageRevision.rawValue) < row revision \(row.messageRevision.rawValue)")
                break
            }
            let targetLifecycle: TranscriptRowLifecycle = terminal.failure.map(TranscriptRowLifecycle.failed) ?? .complete
            // Idempotent: if already in the same terminal state, just update metadata
            if row.lifecycle == targetLifecycle {
                row.displayContent = terminal.displayContent
                row.messageRevision = terminal.messageRevision
                row.lastUpdatedAt = Date()
                replace(row: row)
                state.conversationRevision = max(state.conversationRevision, terminal.conversationRevision)
                break
            }
            guard canTransition(from: row.lifecycle, to: targetLifecycle) else {
                record("illegal_transition", "Cannot transition from \(row.lifecycle) to \(targetLifecycle)")
                break
            }
            row.lifecycle = targetLifecycle
            row.displayContent = terminal.displayContent
            row.messageRevision = terminal.messageRevision
            row.lastUpdatedAt = Date()
            replace(row: row)
            state.conversationRevision = max(state.conversationRevision, terminal.conversationRevision)

        // MARK: Submission rejected
        case let .submissionRejected(rejection, epoch):
            try requireEpoch(epoch); try requireConversation(rejection.conversationID)
            guard let renderID = state.renderIDByClientID[rejection.clientMessageID],
                  var row = state.rowsByRenderID[renderID] else {
                record("missing_client_identity", "Rejection did not match a row by clientMessageID")
                break
            }
            let targetLifecycle = TranscriptRowLifecycle.rejected(rejection.failure)
            guard canTransition(from: row.lifecycle, to: targetLifecycle) else {
                record("illegal_transition", "Cannot transition from \(row.lifecycle) to rejected")
                break
            }
            row.lifecycle = targetLifecycle
            row.lastUpdatedAt = Date()
            replace(row: row)

        // MARK: Explicit deletion (bypasses canTransition — only explicit canonical deletion may remove a row)
        case let .explicitDeletion(deletion, epoch):
            try requireEpoch(epoch); try requireConversation(deletion.conversationID)
            guard var row = state.rowsByRenderID[deletion.renderID] else {
                throw ReducerError.unknownRenderID(deletion.renderID)
            }
            // Idempotent: already deleted is a no-op
            if row.lifecycle == .deleted { break }
            row.lifecycle = .deleted
            row.lastUpdatedAt = Date()
            replace(row: row)
        }
        return makeProjection()
    }

    func projection(for conversationID: CanonicalConversationID) -> TranscriptProjection {
        guard state.activeConversationID == conversationID else { return makeProjection(visible: []) }
        return makeProjection()
    }

    func diagnostics() -> [TranscriptDiagnostic] { rejectionDiagnostics }

    // MARK: - Private helpers

    private func requireEpoch(_ epoch: NavigationEpoch) throws {
        guard epoch == state.navigationEpoch else {
            rejectionDiagnostics.append(TranscriptDiagnostic(category: "stale_navigation_epoch", message: "Event ignored"))
            throw ReducerError.staleNavigationEpoch(expected: state.navigationEpoch, received: epoch)
        }
    }

    private func requireConversation(_ id: CanonicalConversationID) throws {
        guard let active = state.activeConversationID else { state.activeConversationID = id; return }
        guard active == id else {
            rejectionDiagnostics.append(TranscriptDiagnostic(category: "conversation_mismatch", message: "Event ignored", conversationID: id))
            throw ReducerError.conversationMismatch(expected: active, received: id)
        }
    }

    private func record(_ category: String, _ message: String) {
        rejectionDiagnostics.append(TranscriptDiagnostic(category: category, message: message, conversationID: state.activeConversationID))
    }

    private func recordCollision(identity: String, priorRenderID: TranscriptRenderID, eventKind: String, reason: String) {
        rejectionDiagnostics.append(TranscriptDiagnostic(
            category: "identity_collision",
            message: "\(reason) (identity: \(identity), prior: \(priorRenderID.rawValue.uuidString.prefix(8)), event: \(eventKind))",
            conversationID: state.activeConversationID))
    }

    // MARK: - Snapshot reconciliation

    /// Reconcile a snapshot's canonical messages against the reducer's current rows.
    /// - Validates conversation/epoch (caller's responsibility)
    /// - Rejects stale conversation revisions (caller's responsibility)
    /// - Upgrades matched rows in place, preserving renderID
    /// - Creates new render rows for unmatched canonical messages
    /// - Preserves optimistic rows absent from the snapshot
    /// - Marks deleted rows as `.deleted` (does not remove them)
    private func reconcileSnapshot(_ snapshot: TranscriptSnapshot) {
        let previousRevision = state.conversationRevision

        // Collect optimistic row renderIDs for preservation
        var optimisticRenderIDs = Set<TranscriptRenderID>()
        for (_, row) in state.rowsByRenderID {
            if row.lifecycle == .optimistic || row.lifecycle == .submitting {
                optimisticRenderIDs.insert(row.renderID)
            }
        }

        // Track which canonical IDs are covered by this snapshot
        var coveredCanonicalIDs = Set<CanonicalMessageID>()

        for msg in snapshot.canonicalMessages {
            coveredCanonicalIDs.insert(msg.canonicalMessageID)

            // Identity matching: user rows by clientMessageID then canonicalMessageID;
            // assistant rows by canonicalMessageID then jobID
            var matchedRenderID: TranscriptRenderID?

            switch msg.kind {
            case .user:
                // User rows: try clientMessageID first, then canonicalMessageID
                if let clientID = msg.clientMessageID,
                   let rid = state.renderIDByClientID[clientID] {
                    matchedRenderID = rid
                } else if let rid = state.renderIDByCanonicalID[msg.canonicalMessageID] {
                    matchedRenderID = rid
                }
            default:
                // Assistant/tool/reasoning: canonicalMessageID first, then jobID
                if let rid = state.renderIDByCanonicalID[msg.canonicalMessageID] {
                    matchedRenderID = rid
                } else if let jobID = msg.jobID, let rid = state.renderIDByJobID[jobID] {
                    matchedRenderID = rid
                }
            }

            if let renderID = matchedRenderID, var row = state.rowsByRenderID[renderID] {
                // === Per-message revision guard ===
                guard msg.messageRevision >= row.messageRevision else {
                    record("stale_message_revision",
                           "Canonical \(msg.canonicalMessageID.rawValue) revision \(msg.messageRevision.rawValue) < row revision \(row.messageRevision.rawValue)")
                    continue
                }

                // Same revision with conflicting content = contract violation
                if msg.messageRevision == row.messageRevision && msg.displayContent != row.displayContent {
                    record("snapshot_content_conflict",
                           "Same revision \(msg.messageRevision.rawValue) with conflicting content for \(msg.canonicalMessageID.rawValue)")
                    continue
                }

                // Upgrade row in place, preserving renderID
                // Transition lifecycle
                if msg.deleted {
                    row.lifecycle = .deleted
                } else if row.lifecycle == .optimistic || row.lifecycle == .submitting {
                    row.lifecycle = .accepted
                }
                row.canonicalSequence = msg.sequence
                row.messageRevision = msg.messageRevision
                row.displayContent = msg.displayContent
                row.lastUpdatedAt = Date()
                row = TranscriptRow(renderID: row.renderID, canonicalMessageID: msg.canonicalMessageID,
                    clientMessageID: row.clientMessageID ?? msg.clientMessageID, jobID: row.jobID ?? msg.jobID,
                    canonicalSequence: row.canonicalSequence, messageRevision: row.messageRevision,
                    conversationRevisionSeen: row.conversationRevisionSeen, retryGeneration: row.retryGeneration,
                    localOrdinal: row.localOrdinal, kind: row.kind, lifecycle: row.lifecycle,
                    displayContent: row.displayContent, reasoning: row.reasoning, toolActivity: row.toolActivity,
                    attachments: row.attachments, createdAt: row.createdAt, lastUpdatedAt: row.lastUpdatedAt)

                replace(row: row)
                // Remove from optimistic set — it has been upgraded
                optimisticRenderIDs.remove(renderID)
            } else {
                // No matching row — create a new render row
                let now = Date()
                let lifecycle: TranscriptRowLifecycle = msg.deleted ? .deleted : .accepted
                let newRow = TranscriptRow(
                    renderID: TranscriptRenderID(),
                    canonicalMessageID: msg.canonicalMessageID,
                    clientMessageID: msg.clientMessageID,
                    jobID: msg.jobID,
                    canonicalSequence: msg.sequence,
                    messageRevision: msg.messageRevision,
                    conversationRevisionSeen: snapshot.revision,
                    retryGeneration: 0,
                    localOrdinal: LocalOrdinal(rawValue: state.nextLocalOrdinal),
                    kind: msg.kind,
                    lifecycle: lifecycle,
                    displayContent: msg.displayContent,
                    reasoning: nil,
                    toolActivity: nil,
                    attachments: [],
                    createdAt: now,
                    lastUpdatedAt: now)
                state.nextLocalOrdinal &+= 1
                insert(newRow)
            }
        }

        // Update conversation revision
        state.conversationRevision = max(state.conversationRevision, snapshot.revision)
    }

    private func clearRows() {
        state.rowsByRenderID.removeAll()
        state.renderIDByCanonicalID.removeAll()
        state.renderIDByClientID.removeAll()
        state.renderIDByJobID.removeAll()
    }

    private func insert(_ row: TranscriptRow) {
        state.rowsByRenderID[row.renderID] = row
        if let id = row.canonicalMessageID { state.renderIDByCanonicalID[id] = row.renderID }
        if let id = row.clientMessageID { state.renderIDByClientID[id] = row.renderID }
        if let id = row.jobID { state.renderIDByJobID[id] = row.renderID }
    }

    private func replace(row: TranscriptRow, canonicalID: CanonicalMessageID? = nil) {
        state.rowsByRenderID[row.renderID] = row
        if let id = canonicalID { state.renderIDByCanonicalID[id] = row.renderID }
        if let id = row.canonicalMessageID { state.renderIDByCanonicalID[id] = row.renderID }
        if let id = row.clientMessageID { state.renderIDByClientID[id] = row.renderID }
        if let id = row.jobID { state.renderIDByJobID[id] = row.renderID }
    }

    private func replace(row: TranscriptRow) { replace(row: row, canonicalID: row.canonicalMessageID) }
    private func replaceRows(_ rows: [TranscriptRow]) { clearRows(); rows.forEach(insert) }

    private nonisolated func isTerminalOrDeleted(_ lifecycle: TranscriptRowLifecycle) -> Bool {
        switch lifecycle {
        case .complete, .failed, .rejected, .cancelled, .deleted: return true
        default: return false
        }
    }

    private nonisolated func isActiveStreamingLifecycle(_ lifecycle: TranscriptRowLifecycle) -> Bool {
        switch lifecycle { case .submitting, .accepted, .streaming: return true; default: return false }
    }
}

private extension TranscriptReducer {
    func max(_ lhs: ConversationRevision, _ rhs: ConversationRevision) -> ConversationRevision { lhs < rhs ? rhs : lhs }

    func makeProjection(visible overrideVisible: [TranscriptRow]? = nil) -> TranscriptProjection {
        let visible: [TranscriptRow]
        if let override = overrideVisible {
            visible = override
        } else {
            let allRows = Array(state.rowsByRenderID.values)

            // Partition into acknowledged (have canonical sequence) and provisional
            var acknowledged: [TranscriptRow] = []
            var provisional: [TranscriptRow] = []

            for row in allRows {
                if row.canonicalSequence != nil {
                    acknowledged.append(row)
                } else {
                    provisional.append(row)
                }
            }

            // Acknowledged rows sort strictly by canonical sequence
            acknowledged.sort { a, b in
                let seqA = a.canonicalSequence!.rawValue
                let seqB = b.canonicalSequence!.rawValue
                if seqA != seqB { return seqA < seqB }
                // Server contract violation: two acknowledged rows with same canonical sequence
                record("duplicate_canonical_sequence",
                       "Two acknowledged rows share sequence \(seqA): \(a.canonicalMessageID?.rawValue ?? "?") and \(b.canonicalMessageID?.rawValue ?? "?")")
                return a.localOrdinal.rawValue < b.localOrdinal.rawValue
            }

            // Provisional rows sort by local ordinal after the last acknowledged row
            provisional.sort { $0.localOrdinal.rawValue < $1.localOrdinal.rawValue }

            visible = acknowledged + provisional
        }

        // Collect active job IDs (non-terminal, non-deleted rows with a job)
        var activeJobs = Set<JobID>()
        var terminalFailures = [TranscriptRenderID: TranscriptFailure]()

        for row in visible {
            if let jobID = row.jobID {
                if isActiveStreamingLifecycle(row.lifecycle) {
                    activeJobs.insert(jobID)
                }
                if case .failed(let failure) = row.lifecycle {
                    terminalFailures[row.renderID] = failure
                }
                if case .rejected(let failure) = row.lifecycle {
                    terminalFailures[row.renderID] = failure
                }
            }
        }

        // Build cacheable canonical state from acknowledged rows (those with canonicalSequence)
        let canonicalState: Data?
        do {
            let canonicalRows = visible.compactMap { row -> [String: Any]? in
                guard let canonicalID = row.canonicalMessageID,
                      let sequence = row.canonicalSequence else { return nil }
                return [
                    "canonicalMessageId": canonicalID.rawValue,
                    "sequence": sequence.rawValue,
                    "messageRevision": row.messageRevision.rawValue,
                    "displayContent": row.displayContent,
                    "lifecycle": String(describing: row.lifecycle)
                ]
            }
            canonicalState = try JSONSerialization.data(withJSONObject: canonicalRows)
        } catch {
            canonicalState = nil
        }

        return TranscriptProjection(
            activeConversationID: state.activeConversationID,
            navigationEpoch: state.navigationEpoch,
            conversationRevision: state.conversationRevision,
            orderedVisibleRows: visible,
            activeJobIDs: activeJobs,
            terminalFailuresByRenderID: terminalFailures,
            cacheableCanonicalState: canonicalState
        )
    }
}

// MARK: - Type aliases

typealias TranscriptSnapshot = TranscriptReducer.TranscriptSnapshot
typealias CanonicalMessage = TranscriptReducer.CanonicalMessage
typealias OptimisticUserSubmission = TranscriptReducer.OptimisticUserSubmission
typealias UserSubmissionAcceptance = TranscriptReducer.UserSubmissionAcceptance
typealias UserSubmissionRetrying = TranscriptReducer.UserSubmissionRetrying
typealias AssistantJobBinding = TranscriptReducer.AssistantJobBinding
typealias TranscriptStreamDelta = TranscriptReducer.TranscriptStreamDelta
typealias TranscriptTerminalEvent = TranscriptReducer.TranscriptTerminalEvent
typealias SubmissionRejection = TranscriptReducer.SubmissionRejection
typealias TranscriptDeletion = TranscriptReducer.TranscriptDeletion
typealias TranscriptProjection = TranscriptReducer.TranscriptProjection
// Event is namespaced by the actor to keep the reducer's public state machine clear.
