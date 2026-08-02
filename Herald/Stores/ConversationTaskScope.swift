import Foundation

/// Coordinates task ownership and epoch lifecycle for transcript navigation.
///
/// `ConversationTaskScope` is a thin coordinator that sits between ChatStore and
/// `TranscriptReducer`.  It does NOT replace the reducer -- the reducer remains the
/// single source of truth for visible state.  The scope's job is to:
///
/// 1. Own every Task launched for the active conversation (load, streams, poll, cache writes).
/// 2. Cancel those tasks atomically when navigation changes.
/// 3. Propagate the epoch to the reducer so stale-epoch rejection fires as defence-in-depth.
/// 4. Support per-await re-validation so that no suspension point can silently deliver
///    a result belonging to a previous conversation or epoch.
///
/// ## Navigation transition flow (correction section 10)
///
/// On conversation switch, new chat, disconnect, sign-out, or view-model teardown:
/// 1. Increment `navigationEpoch`.
/// 2. Cancel every owned task (`loadTask`, each `streamTasks[id]`, `pollTask`, `cacheWriteTask`).
/// 3. Submit `.deactivateConversation(oldEpoch)` to the reducer.
/// 4. Submit `.activateConversation(newID, newEpoch)` to the reducer.
/// 5. Start cache hydration for the new conversation using the new epoch.
/// 6. Start the server load task, capturing the new epoch in a local constant.
/// 7. After every `await`, check `Task.isCancelled`, captured epoch == scope's current epoch,
///    and captured conversation ID == scope's current conversation ID.
/// 8. Submit results to the reducer with the captured epoch.
///
/// ## Background idle state
///
/// When the app is backgrounded and a long-running stream is in flight, the scope
/// continues running (server work is durable).  On background -> foreground transition,
/// no navigation epoch change occurs; the visible projection simply catches up via the
/// next snapshot.  If the app is force-killed, the scope's in-memory state is lost.
/// On relaunch, ChatStore hydrates from the reducer's `cacheableCanonicalState`
/// and resumes via an authoritative snapshot.
///
/// ## Queueing support
///
/// The app supports queueing or sending a new turn without cancelling the current
/// server run.  When the user sends a message while another is streaming, the
/// optimistic user row is added to the reducer immediately and the send task is
/// started, but the previous stream task is NOT cancelled.  If the server protocol
/// serializes turns, the queued state is displayed and the message is preserved
/// locally.  The reducer accepts multiple `.optimisticUserSubmitted` events for the
/// same conversation without losing ordering (each gets its own renderID).
actor ConversationTaskScope {

    // MARK: - Active state

    /// The canonical conversation ID the user is currently viewing.
    private(set) var activeConversationID: CanonicalConversationID?

    /// Monotonically increasing navigation generation.  Incremented on every
    /// navigation change; checked by the reducer as defence-in-depth.
    private(set) var navigationEpoch: NavigationEpoch

    // MARK: - Owned tasks

    /// The task that loads the authoritative snapshot for the active conversation.
    private var loadTask: Task<Void, Never>?

    /// Per-job stream coordinator tasks.  Keyed by `JobID` so that cancelling
    /// a single job's stream is possible without tearing down other streams.
    private var streamTasks: [JobID: Task<Void, Never>] = [:]

    /// Long-poll / cursor-poll task for the active conversation.
    private var pollTask: Task<Void, Never>?

    /// Background cache-write task.
    private var cacheWriteTask: Task<Void, Never>?

    /// Send tasks keyed by `ClientMessageID`.  Tracked so they can be cancelled
    /// on navigation if needed, but per the queueing contract the previous send
    /// is NOT cancelled when a new send begins.
    private var sendTasks: [ClientMessageID: Task<Void, Never>] = [:]

    // MARK: - Internal reducer reference

    private let reducer: TranscriptReducer

    // MARK: - Init

    init(reducer: TranscriptReducer) {
        self.reducer = reducer
        self.navigationEpoch = .zero
    }

    // MARK: - Public queries

    /// Returns the current projection from the reducer for the given conversation.
    func projection(for conversationID: CanonicalConversationID) async -> TranscriptProjection {
        await reducer.projection(for: conversationID)
    }

    /// Returns the reducer's rejection diagnostics.
    func diagnostics() async -> [TranscriptDiagnostic] {
        await reducer.diagnostics()
    }

    // MARK: - Navigation transition

    /// Performs the full navigation transition described in correction section 10.
    ///
    /// - Parameters:
    ///   - newConversationID: The conversation being navigated to.
    ///   - snapshotLoader: An async closure that fetches the authoritative snapshot
    ///     for `newConversationID`.  The closure receives the captured epoch and
    ///     conversation ID and must check them after every suspension point.
    func navigateTo(
        _ newConversationID: CanonicalConversationID,
        snapshotLoader: @Sendable @escaping (NavigationEpoch, CanonicalConversationID) async -> TranscriptReducer.TranscriptSnapshot?
    ) async {
        // 1. Capture old state for deactivation
        let oldEpoch = navigationEpoch
        let oldConversationID = activeConversationID

        // 2. Increment epoch
        navigationEpoch = navigationEpoch.incremented()
        let newEpoch = navigationEpoch

        // 3. Cancel every owned task
        cancelAllTasks()

        // 4. Deactivate old conversation in the reducer (best-effort)
        if oldConversationID != nil {
            _ = try? await reducer.reduce(.deactivateConversation(oldEpoch))
        }

        // 5. Activate new conversation in the reducer
        activeConversationID = newConversationID
        _ = try? await reducer.reduce(.activateConversation(newConversationID, newEpoch))

        // 6-8. Start the server load task with per-await re-validation
        loadTask = Task { [weak self] in
            guard let self else { return }

            // Capture epoch and conversation ID at the time of launch
            let capturedEpoch = newEpoch
            let capturedID = newConversationID

            // Await the snapshot loader
            let snapshot = await snapshotLoader(capturedEpoch, capturedID)

            // Per-await re-validation
            guard !Task.isCancelled else { return }
            let currentEpoch = await self.navigationEpoch
            let currentID = await self.activeConversationID
            guard currentEpoch == capturedEpoch,
                  currentID == capturedID else { return }

            // Submit to reducer with captured epoch
            if let snapshot {
                _ = try? await self.reducer.reduce(.snapshotReceived(snapshot, capturedEpoch))
            }
        }
    }

    // MARK: - Stream task management

    /// Registers a stream coordinator task for a specific job.
    /// Does NOT cancel existing stream tasks (queueing support: multiple concurrent
    /// streams are allowed for the same conversation).
    func registerStreamTask(_ task: Task<Void, Never>, for jobID: JobID) {
        streamTasks[jobID] = task
    }

    /// Removes a stream task after it completes.
    func removeStreamTask(for jobID: JobID) {
        streamTasks.removeValue(forKey: jobID)
    }

    // MARK: - Send task management (queueing support)

    /// Registers a send task.  Previous send tasks for the same conversation are
    /// NOT cancelled (the server serializes turns; the UI shows queued state).
    func registerSendTask(_ task: Task<Void, Never>, for clientMessageID: ClientMessageID) {
        sendTasks[clientMessageID] = task
    }

    /// Removes a send task after it completes.
    func removeSendTask(for clientMessageID: ClientMessageID) {
        sendTasks.removeValue(forKey: clientMessageID)
    }

    // MARK: - Poll and cache task management

    func setPollTask(_ task: Task<Void, Never>?) {
        pollTask?.cancel()
        pollTask = task
    }

    func setCacheWriteTask(_ task: Task<Void, Never>?) {
        cacheWriteTask?.cancel()
        cacheWriteTask = task
    }

    // MARK: - Cancellation

    /// Cancels all owned tasks.  Called during navigation transitions.
    /// Durable server work is NOT cancelled -- only client-side tasks that
    /// feed results into the reducer.
    func cancelAllTasks() {
        loadTask?.cancel()
        loadTask = nil
        for (_, task) in streamTasks { task.cancel() }
        streamTasks.removeAll()
        pollTask?.cancel()
        pollTask = nil
        cacheWriteTask?.cancel()
        cacheWriteTask = nil
        // Note: sendTasks are intentionally NOT cancelled here.
        // Per the queueing contract, if turns are serialized server-side,
        // the send must complete so the message is preserved locally.
    }

    /// Full teardown: cancels everything including send tasks.
    /// Use for sign-out or explicit user-initiated stop.
    func teardown() {
        cancelAllTasks()
        for (_, task) in sendTasks { task.cancel() }
        sendTasks.removeAll()
        activeConversationID = nil
    }

    // MARK: - Per-await validation helpers

    /// Validates that the current scope state matches captured values.
    /// Returns `true` if the result is still valid for reduction.
    func validateAfterSuspension(
        capturedEpoch: NavigationEpoch,
        capturedConversationID: CanonicalConversationID
    ) async -> Bool {
        guard !Task.isCancelled else { return false }
        let currentEpoch = self.navigationEpoch
        let currentID = self.activeConversationID
        return currentEpoch == capturedEpoch && currentID == capturedConversationID
    }

    /// Submits a navigation-epoch-guarded event to the reducer.
    /// Returns the projection if the event was accepted, nil otherwise.
    func submitEvent(_ event: TranscriptReducer.Event) async -> TranscriptProjection? {
        try? await reducer.reduce(event)
    }

    // MARK: - Stale-result rejection test helper

    /// Simulates a late result arriving after navigation.
    /// Returns true if the event was rejected (stale epoch or conversation mismatch).
    func isRejectedByReducer(_ event: TranscriptReducer.Event) async -> Bool {
        do {
            _ = try await reducer.reduce(event)
            return false
        } catch TranscriptReducer.ReducerError.staleNavigationEpoch,
                TranscriptReducer.ReducerError.conversationMismatch {
            return true
        } catch {
            return false
        }
    }
}
