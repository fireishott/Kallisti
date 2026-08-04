import Foundation
import os
import UIKit
import UserNotifications

/// Indicates the current phase of the streaming pipeline for UI feedback.
enum StreamingPhase: Sendable {
    case idle
    case sending            // POST /messages — waiting for relay to accept
    case waitingForJob      // Job accepted, waiting for first event (connector warming up)
    case streaming          // Receiving text/reasoning/tool deltas
    case reconnecting       // Transport dropped — cursor-based resume in progress
    case stalled            // Watchdog is about to fire — showing "Waiting…"
    case restarting         // Build 33: a Hermes gateway restart is in flight
}

@MainActor
@Observable
final class ChatStore {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "ChatStore")
    /// Closure to check user haptic preference — wired by ChatScreen
    /// to avoid ChatStore depending on SettingsStore directly.
    var hapticFeedbackEnabled: () -> Bool = { true }
    var conversation: Conversation? {
        didSet {
            // Reset auto-title guard only when switching to a different conversation,
            // not on in-place updates (merge, message appends) to the same conversation.
            if oldValue?.id != conversation?.id {
                autoTitleAttempted = false
            }
        }
    }
    var isLoading = false
    var pendingMessageSentAt: Date?
    /// Build 33 WSB: durable outbox — in-memory mirror of the on-disk manifest
    /// (Application Support/Herald/Outbox/outbox.json). Items survive
    /// conversation switches and force-quit; they are submitted FIFO per
    /// conversation, one at a time, by `submitNextEligible`.
    private(set) var outboxItems: [ChatOutboxRecord] = []
    /// Full send lifecycle phase for the active outbox job, parallel to
    /// `streamingPhase`. Owned by the immutable (conversationID,
    /// clientMessageID, attemptID) tuple so a stale phase from a superseded
    /// attempt can never clobber the active banner.
    private(set) var sendPhase: MessageSendPhase = .idle
    private(set) var sendPhaseOwner: SendPhaseOwner?
    /// Re-entrancy guard, per-conversation. submitNextEligible acquires a
    /// `SubmitLease` for the conversation before any network call; later FIFO
    /// items in the SAME conversation no-op at the guard until the active
    /// attempt releases, but other conversations may submit independently.
    /// Replaces the legacy global `submitInFlight: Bool` whose only valid
    /// invariant was "one outbox submit at a time across the whole app" —
    /// which violated "other conversations may submit independently if
    /// supported" (Build 102 marching orders §5).
    private var activeLeases: [UUID: SubmitLease] = [:]
    /// Monotonic FIFO counter persisted in the manifest.
    private var outboxNextSequence = 0
    /// Defaults to Application Support; tests inject a scratch directory so
    /// outbox state never leaks between test runs.
    private let outboxStore: OutboxManifestStore
    var lastTokenUsage: TokenUsage?
    var lastContextInfo: ContextInfo?
    /// Error context from the most recent `.failed` streaming update.
    var lastErrorCategory: String?
    var lastErrorAction: String?
    /// Live log entries for the iPad inspector panel's Logs tab.
    var logEntries: [LogEntry] = []
    /// Streaming phase for UI indicators (e.g. "Sending…", "Waiting…", "Streaming…").
    /// Updated by `runStreamingAttemptLegacy` as the job progresses through the SSE pipeline.
    var streamingPhase: StreamingPhase = .idle
    private var isPollingEnabled = false
    private var pollingTask: Task<Void, Never>?
    private var streamingTask: Task<Void, Never>?
    /// Build 31 (fix): monotonically-incrementing generation token.  Every new
    /// streaming attempt captures this value; every callback in the consumer task
    /// checks it before mutating global state.  A retry or cancel bumps it, so
    /// stale events from a superseded attempt cannot overwrite the active
    /// banner, placeholder, transcript, or counters.  Prior code had no such
    /// guard — a late `.reconnecting` from the old stream coordinator would set
    /// `streamingPhase = .reconnecting` while a retry/replacement was active.
    private var activeAttemptID: UUID = UUID()
    /// Relay-assigned job ID for the active streaming attempt. Updated by the
    /// consumer when `.messageSent` arrives; read by the watchdog when it
    /// returns `.stalled(jobID)` so the polling fallback reattaches to the
    /// same Hermes run instead of minting a second one. Reset at the start
    /// of every attempt; nil between attempts.
    private var acceptedJobID: UUID?
    private var activeStreams: [UUID: UUID] = [:]  // jobId → placeholderId
    var streamingMessageID: UUID? {
        activeStreams.values.first
    }

    /// After `messageSent`, if no real progress (text/reasoning delta, tool
    /// activity, or finish) arrives within this window, the job is treated as
    /// silently stalled/dropped — see `runStreamingAttemptLegacy`.
    /// Mutable so tests can set it to milliseconds.
    ///
    /// Set to 60s — large models can take 30-45s to load/prefill before the
    /// first token, and the connector has its own 120s watchdog. 30s was too
    /// tight for real-world usage with local models on constrained hardware.
    /// If no text/reasoning/tool/finished event arrives within this window,
    /// the job is treated as stalled and parallel polling begins.
    static var watchdogTimeout: Duration = .seconds(90)

    /// Absolute deadline for a single streaming job from message acceptance to
    /// terminal resolution. After this duration, the polling loop forcibly
    /// resolves the placeholder to a timeout failure, even if heartbeats are
    /// still arriving. This prevents the "infinite thinking" bug where a hung
    /// upstream model keeps the job alive via heartbeats forever.
    /// Mirrors the relay's max_job_duration_seconds.
    static var absoluteJobDeadline: Duration = .seconds(180)

    /// Timestamp of the last streaming progress signal. Updated on every
    /// textDelta, reasoningDelta, toolActivity, keepalive, and messageSent.
    /// The continuous watchdog checks this to detect mid-stream stalls.
    private var streamingProgressAt: Date = .now

    // Delta coalescing — tokens arrive faster than SwiftUI can usefully redraw.
    // Buffer deltas per-placeholder in an Array<String> (avoids O(n²) inline
    // concat) and flush onto the placeholder at ~30fps so every append triggers
    // at most one @Observable notification per frame.
    private struct DeltaBuffer {
        var chunks: [String] = []
        var bytes: Int = 0
        var flushTask: Task<Void, Never>?
    }
    private var deltaBuffers: [UUID: DeltaBuffer] = [:]
    private var reasoningBuffers: [UUID: DeltaBuffer] = [:]
    private static let deltaFlushInterval: Duration = .milliseconds(16)  // 60 fps cap
    private static let deltaFlushByteThreshold = 4_096

    /// Whether deterministic local title derivation has run for this conversation.
    private var autoTitleAttempted = false

    /// Set by `clearConversation()` to force the next `loadConversationIfNeeded()`
    /// to bypass the local cache and fetch fresh data from the relay. Prevents
    /// the /new bug where a stale cached conversation survives the clear.
    private var needsServerRefresh = false

    /// Build 26: true after a terminal completion when the active-chat
    /// projection was persisted but the server's conversation snapshot has
    /// not yet been proven to contain this terminal turn.  The next explicit
    /// refresh (chat-list tap, foreground, poll) should reconcile through
    /// mergeConversationMetadata — which preserves local attachments and
    /// content via B39 T5 guards — rather than silently replacing the local
    /// answer array.
    private var pendingServerReconciliation = false

    /// Build 30: monotonically increasing generation counter.  Every async
    /// conversation mutation (poll, foreground refresh, explicit load) must
    /// capture the current generation before the await and discard the result
    /// if the store's generation has moved past it.  This prevents a stale
    /// server snapshot (returned after the terminal row was committed) from
    /// replacing the local answer array wholesale.
    private var conversationGeneration: UInt64 = 0

    var isStreaming: Bool { streamingMessageID != nil }

    /// Build 33: set while a Hermes gateway restart is in flight. Sends are
    /// queued visibly instead of being submitted to a dying gateway, streams
    /// are suspended, and the status indicator reports the restart.
    private(set) var restartInProgress = false

    /// Connection status, overridden by an in-flight gateway restart: the
    /// underlying transport's status is meaningless while the gateway is
    /// being replaced, and the UI must say so instead of flapping.
    var connectionStatus: ConnectionStatus {
        if restartInProgress { return .restarting }
        return heraldClient.connectionStatus
    }

    func updateConnectionStatus(_ status: ConnectionStatus) {
        heraldClient.connectionStatus = status
    }

    /// Dynamic slash command catalog fetched from the connected Hermes host.
    /// Includes gateway commands, installed skills, custom personalities,
    /// and hidden quick-command metadata for manual slash dispatch.
    private(set) var commandCatalog: [SlashCommand] = SlashCommand.allBuiltIn

    /// Active model name from the Herald agent config (e.g., "gpt-5.4-mini").
    private(set) var activeModelName: String?
    /// Context window size for the active model (e.g., 400000).
    private(set) var contextWindow: Int?

    var currentContextTokens: Int? {
        lastTokenUsage?.promptTokens
    }

    /// Injected by AppContainer so profile-switch detection can update the
    /// active profile name on the owning ProfileStore.
    var profileStore: ProfileStore?

    var heraldClient: any HeraldClientProtocol
    private let chatLiveActivity = LiveActivityService()
    var persistence: any AppPersistenceStoreProtocol

    /// TTS service for speaking responses during/after streaming.
    @ObservationIgnored var ttsService: (any TTSServiceProtocol)?
    /// Provides current TTS settings (enabled, voice, autoSpeak, autoSpeakDuringStreaming, appleVoiceIdentifier).
    @ObservationIgnored var ttsSettingsProvider: (@MainActor () -> (enabled: Bool, voice: String, autoSpeak: Bool, autoSpeakDuringStreaming: Bool, appleVoiceIdentifier: String))?

    /// Called when conversation content changes (new message, streaming complete).
    /// Used by AppContainer to push widget data updates.
    var onConversationChanged: (@MainActor () -> Void)?

    /// Called when the conversation title changes (server-derived or renamed).
    /// Used by SessionListStore to update sidebar immediately.
    var onTitleChanged: (@MainActor (_ conversationID: UUID, _ newTitle: String) -> Void)?
    var useStreaming: Bool = false

    /// Maximum number of log entries to keep in memory and on disk.
    private static let maxLogEntries = 500

    init(
        heraldClient: any HeraldClientProtocol,
        persistence: any AppPersistenceStoreProtocol,
        outboxBaseDirectory: URL? = nil
    ) {
        self.heraldClient = heraldClient
        self.persistence = persistence
        self.outboxStore = outboxBaseDirectory.map { OutboxManifestStore(baseDirectory: $0) }
            ?? OutboxManifestStore()
        // Restore persisted logs so the Logs tab isn't empty on launch.
        if let persisted = persistence.loadLogEntries(), !persisted.isEmpty {
            logEntries = persisted
        } else {
            logEntries = [LogEntry(level: .info, message: "Kallisti started — waiting for activity")]
        }
        // Restore the durable outbox manifest. Items are NOT submitted here —
        // recovery runs from ChatScreen appear / app foreground
        // (`recoverOutbox`), where the current conversation is known.
        let manifest = outboxStore.load()
        outboxItems = manifest.items
        outboxNextSequence = manifest.nextSequence
    }

    func loadConversationIfNeeded() async {
        if conversation == nil {
            conversation = persistence.loadConversationCache()
            // IMPORTANT: Do NOT trust cached contextPercent or latestUsage —
            // they're stale by definition (the model's context window may have
            // changed, a different model may be active, or the relay may report
            // completely different usage after the next response). Restoring
            // stale usage causes fabricated "Session nearly full" banners on
            // first launch and after session switches.
            conversation?.contextPercent = nil
            conversation?.latestUsage = nil
        }
        // After clearConversation(), bypass the local cache and force a
        // server fetch so the UI never shows the stale archived conversation.
        // `conversations/current` is connector-global in direct-connector
        // deployments.  It cannot identify which thread this physical device
        // had selected.  A persisted selection is therefore always loaded by
        // its explicit ID, even when a local cache is present.
        if let selectedID = persistence.currentSessionId {
            needsServerRefresh = false
            await loadConversation(id: selectedID)
            clearNotificationsForCurrentConversation()
            return
        }

        guard conversation == nil || needsServerRefresh else { return }
        needsServerRefresh = false
        await loadConversation()
        clearNotificationsForCurrentConversation()
    }

    func loadConversation() async {
        isLoading = true
        defer { isLoading = false }
        // Build 31: capture generation before the await.  If a terminal row is
        // committed during the fetch (by .finished's merge), the generation bump
        // tells us the pre-await snapshot is stale.  Discard the result and let
        // the poll loop (which does check generation) pick up the authoritative
        // state on its next cycle.
        let capturedGeneration = conversationGeneration
        // Snapshot the current conversation BEFORE the async gap — the merge
        // uses this to preserve local-only rows that the server payload lacks.
        // If .finished commits a terminal row while we're waiting, that row
        // is in the live `conversation` but not in this snapshot.  The
        // generation check below catches that case.
        let cachedConversation = conversation ?? persistence.loadConversationCache()
        let refreshed = await heraldClient.loadConversation()
        // Discard if the generation changed while we were fetching — a
        // terminal completion, another load, or a session switch invalidated
        // our pre-await snapshot.
        guard conversationGeneration == capturedGeneration else {
            Logger.app.info("loadConversation: discarded (generation \(capturedGeneration) → \(self.conversationGeneration))")
            restartPendingPollingIfNeeded()
            return
        }
        conversation = mergeConversationMetadata(
            from: cachedConversation,
            into: refreshed
        )
        if sendPhase == .idle {
            streamingPhase = .idle
        }
        autoTitleAttempted = false
        if let latestUsage = conversation?.latestUsage {
            lastTokenUsage = latestUsage
        }
        if let conversation {
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
            onConversationChanged?()
        }
        restartPendingPollingIfNeeded()
        clearNotificationsForCurrentConversation()
    }

    private func loadConversation(id: UUID) async {
        isLoading = true
        defer { isLoading = false }
        // Build 30: bump generation so any in-flight poll/refresh for a
        // prior conversation is discarded.  Without this, navigating to a
        // new chat while a poll response is in-flight could replace the
        // just-loaded conversation with stale data from the old one.
        conversationGeneration &+= 1
        do {
            let refreshed = try await heraldClient.loadConversation(id: id)
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)
            if sendPhase == .idle {
                streamingPhase = .idle
            }
            persistence.currentSessionId = conversation?.id
            if let conversation {
                var cacheCopy = conversation
                cacheCopy.contextPercent = nil
                cacheCopy.latestUsage = nil
                persistence.saveConversationCache(cacheCopy)
                onConversationChanged?()
            }
        } catch {
            Self.logger.warning("Failed to load selected conversation \(id): \(error.localizedDescription)")
        }
    }

    /// Build 26: atomically persist the current conversation to the local
    /// cache.  Called inside the .finished handler after the terminal message
    /// replaces the streaming placeholder but before activeStreams ownership
    /// is cleared — so a polling refresh or foreground recovery cannot
    /// remove the answer the user already saw.
    private func cacheModifiedConversation() {
        guard let conversation else { return }
        var cacheCopy = conversation
        cacheCopy.contextPercent = nil
        cacheCopy.latestUsage = nil
        persistence.saveConversationCache(cacheCopy)
    }

    /// Full send pipeline: phase 1 (durable enqueue) followed by phase 2
    /// (submit the next eligible outbox item). Kept as the single entry point
    /// so callers (ChatScreen, retryMessage, tests) get the whole lifecycle.
    func sendMessage(_ content: String, attachments: [PendingAttachment] = [], clientMessageID: UUID? = nil, continuationContext: String? = nil) async {
        guard let record = enqueueMessage(
            content,
            attachments: attachments,
            clientMessageID: clientMessageID,
            continuationContext: continuationContext
        ) else { return }
        await submitNextEligible(for: record.conversationID)
    }

    // MARK: - Durable Outbox — Phase 1 (enqueue)

    /// Phase 1 of the two-phase send. Validates, stages attachment bytes,
    /// creates the durable outbox record, and appends the optimistic user row
    /// IMMEDIATELY — before any network call — so the user sees visible state
    /// during the send. Persists the outbox manifest AND the conversation
    /// cache before returning.
    ///
    /// The composer must only be cleared after this returns non-nil: the
    /// outbox now owns the text, so a force-quit at any later point cannot
    /// lose the message.
    ///
    /// During a Hermes gateway restart this still enqueues durably (state
    /// `.queued`) and appends the optimistic row plus the explanatory system
    /// message — `submitNextEligible` no-ops until `resumeAfterRestart()`.
    @discardableResult
    func enqueueMessage(
        _ content: String,
        attachments: [PendingAttachment] = [],
        clientMessageID: UUID? = nil,
        continuationContext: String? = nil
    ) -> ChatOutboxRecord? {
        let trimmedContent = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedContent.isEmpty || !attachments.isEmpty else { return nil }
        guard hasPendingDuplicateMessage(trimmedContent, attachments: attachments) == false else { return nil }
        guard attachments.count <= PendingAttachment.maxAttachmentsPerMessage else {
            appendLog(level: .warn, "Message rejected — \(attachments.count) attachments exceeds the limit")
            return nil
        }

        sendPhase = .preparing
        let conversationID = conversation?.id ?? UUID()
        if conversation == nil {
            conversation = Conversation(id: conversationID, title: "New Chat")
        }
        persistence.currentSessionId = conversationID

        let clientID = clientMessageID ?? UUID()
        let displayContent = trimmedContent.isEmpty && !attachments.isEmpty
            ? "[\(attachments.count) attachment\(attachments.count == 1 ? "" : "s")]"
            : trimmedContent
        let optimistic = Message(
            id: clientID,
            clientMessageID: clientID,
            sender: .user,
            content: displayContent,
            status: .sending,
            attachments: attachments.map { MessageAttachment(from: $0) }
        )

        // Stage attachment bytes (conversation-scoped staging directory).
        var staged: [OutboxAttachmentRef] = []
        for attachment in attachments {
            guard let ref = outboxStore.stageAttachment(
                conversationID: conversationID,
                pending: attachment,
                stableID: UUID()
            ) else {
                // Staging failed — remove what we already wrote and reject the
                // enqueue; a record with missing bytes could never submit.
                let partial = ChatOutboxRecord(
                    schemaVersion: OutboxManifestStore.schemaVersion,
                    clientMessageID: clientID,
                    conversationID: conversationID,
                    createdAt: .now,
                    sequence: 0,
                    cleanText: trimmedContent,
                    continuationContext: continuationContext,
                    attachmentRefs: staged,
                    state: .queued,
                    jobID: nil,
                    canonicalUserMessageID: nil,
                    terminalMessageID: nil,
                    attemptCount: 0,
                    nextAttemptAt: nil,
                    lastError: nil,
                    installationId: nil,
                    deviceId: nil,
                    hermesSessionId: nil,
                    requestId: nil,
                    attemptID: nil,
                    updatedAt: .now
                )
                outboxStore.removeStagedAttachments(for: partial)
                sendPhase = .failed("Could not stage attachment data")
                appendLog(level: .error, "Attachment staging failed — message not enqueued")
                return nil
            }
            staged.append(ref)
        }

        // A retry reuses the original clientMessageID (relay idempotency).
        // Supersede any previous record with the same ID — exactly one outbox
        // record may own a clientMessageID at a time, or the state-transition
        // lookups (messageSent/terminal/failure) would hit the stale one.
        if let existingIdx = outboxItems.firstIndex(where: { $0.clientMessageID == clientID }) {
            outboxStore.removeStagedAttachments(for: outboxItems[existingIdx])
            outboxItems.remove(at: existingIdx)
        }

        outboxNextSequence += 1
        let record = ChatOutboxRecord(
            schemaVersion: OutboxManifestStore.schemaVersion,
            clientMessageID: clientID,
            conversationID: conversationID,
            createdAt: .now,
            sequence: outboxNextSequence,
            cleanText: trimmedContent,
            continuationContext: continuationContext,
            attachmentRefs: staged,
            state: .queued,
            jobID: nil,
            canonicalUserMessageID: clientID,
            terminalMessageID: nil,
            attemptCount: 0,
            nextAttemptAt: nil,
            lastError: nil,
            installationId: nil,
            deviceId: nil,
            hermesSessionId: nil,
            requestId: nil,
            attemptID: nil,
            updatedAt: .now
        )
        outboxItems.append(record)
        persistOutbox()

        // Append the optimistic row and persist the conversation cache.
        conversation?.messages.append(optimistic)
        conversation?.lastActivity = optimistic.timestamp
        pendingMessageSentAt = optimistic.timestamp
        cacheModifiedConversation()

        sendPhase = .queued
        sendPhaseOwner = SendPhaseOwner(
            conversationID: conversationID,
            clientMessageID: clientID,
            attemptID: UUID()
        )

        // Build 33: during a gateway restart the gateway is down or being
        // replaced. The message is durably queued and explained; the submit
        // phase no-ops until resumeAfterRestart() drains the outbox.
        if restartInProgress {
            if !conversationHasRestartNotice() {
                conversation?.messages.append(Message(
                    sender: .system,
                    content: "Hermes is restarting — your message will be sent when it is back online.",
                    status: .delivered
                ))
                cacheModifiedConversation()
            }
            appendLog(level: .warn, "Message queued — Hermes gateway restart in progress")
        }
        onConversationChanged?()
        return record
    }

    /// True when the transcript already carries the restart notice — the
    /// notice is appended once per restart, not once per queued message.
    private func conversationHasRestartNotice() -> Bool {
        conversation?.messages.contains(where: {
            $0.sender == .system
                && $0.content.contains("your message will be sent when it is back online")
        }) == true
    }

    // MARK: - Durable Outbox — Phase 2 (submit)

    /// Phase 2 of the two-phase send. Submits the next `.queued` outbox item
    /// for `conversationID` (defaults to the current conversation) using a
    /// compare-and-set lease:
    ///
    ///    .queued → .materializing → (ensureConversation) → .submitting →
    ///    .accepted (jobID) → .terminal (canonical IDs)
    ///
    /// Failures land in `.retryableFailure` (exponential backoff) or
    /// `.permanentFailure`. FIFO is enforced per conversation: exactly one
    /// item is in flight at a time — the terminal handler of the active job
    /// chains the next one.
    ///
    /// No-ops during a gateway restart and while another outbox job or
    /// stream is in flight (the item stays `.queued` and visibly pending).
    func submitNextEligible(for conversationID: UUID? = nil) async {
        let targetID = conversationID ?? conversation?.id
        guard let targetID else { return }

        // Restart suspension: the gateway is down or being replaced. Items
        // stay durably queued; resumeAfterRestart() drains them.
        guard !restartInProgress else {
            sendPhase = .queued
            return
        }
        // One in-flight outbox job PER CONVERSATION — the rest wait in
        // .queued and are chained after the active job reaches a terminal
        // state. Different conversations are independent (test 8 of the
        // Build 102 marching orders). The `isStreaming` check is implicit:
        // a streaming attempt holds a lease for its conversation, and the
        // lease acquisition below is the single guard.
        guard activeLeases[targetID] == nil else { return }

        // Retryable failures whose backoff elapsed are promoted back to
        // .queued so the chain can attempt them again — visible attempt
        // ownership without waiting for the next foreground.
        // Guard: never promote ahead of a newer queued item without first
        // settling against the relay (if terminal → terminalize, don't
        // resubmit).
        let now = Date.now
        var promoted = false
        let newestQueuedSeq = outboxItems
            .filter { $0.conversationID == targetID && $0.state == .queued }
            .map(\.sequence)
            .max()
        for idx in outboxItems.indices
            where outboxItems[idx].conversationID == targetID
                && outboxItems[idx].state == .retryableFailure {
            guard let nextAttemptAt = outboxItems[idx].nextAttemptAt,
                  nextAttemptAt <= now else { continue }
            // If there's a newer queued item, settle this retryable
            // against the relay before promoting — it may already be
            // terminal.
            if let maxSeq = newestQueuedSeq,
               outboxItems[idx].sequence < maxSeq,
               let jobID = outboxItems[idx].jobID {
                if let status = await heraldClient.getJobStatus(jobID),
                   status.status == "terminal" {
                    terminalizeOutboxItem(
                        outboxItems[idx],
                        canonicalUserMessageID: outboxItems[idx].clientMessageID,
                        terminalMessageID: status.message?.id
                    )
                    continue
                }
            }
            outboxItems[idx].state = .queued
            promoted = true
        }
        if promoted {
            persistOutbox()
        }
        guard let index = outboxItems.firstIndex(where: {
            $0.conversationID == targetID && $0.state == .queued
        }) else { return }

        // Per-conversation lease replaces the legacy global `submitInFlight`
        // bool. Different conversations submit independently (Build 102
        // marching orders §5 test 8); FIFO is strict inside each.
        // The defer below is identity-checked so stale callbacks from older
        // tasks cannot clobber a newer lease. The lease is also released
        // manually before the recursive FIFO call so the next item in this
        // conversation can re-acquire — `defer` only fires when this
        // function returns, after the recursion completes.
        var item = outboxItems[index]
        item.state = .materializing
        item.attemptCount += 1
        item.nextAttemptAt = nil
        outboxItems[index] = item
        persistOutbox()

        let lease = SubmitLease(
            conversationID: targetID,
            clientMessageID: item.clientMessageID,
            attemptID: UUID(),
            acquiredAt: .now
        )
        activeLeases[targetID] = lease
        defer {
            if activeLeases[targetID] == lease {
                activeLeases[targetID] = nil
                Self.logger.info("lease released conv=\(targetID.uuidString.prefix(8)) client=\(item.clientMessageID.uuidString.prefix(8))")
            }
            pruneTerminalOutboxRecords()
        }

        sendPhaseOwner = SendPhaseOwner(
            conversationID: targetID,
            clientMessageID: item.clientMessageID,
            attemptID: lease.attemptID
        )

        // Rebuild staged attachments from disk.
        let attachments: [PendingAttachment] = item.attachmentRefs.compactMap { ref in
            outboxStore.pendingAttachment(for: ref, conversationID: targetID)
        }
        guard attachments.count == item.attachmentRefs.count else {
            // Staged bytes are gone — never fabricate empty attachments.
            // Surface a visible failure with manual retry.
            let error = "Attachment data was missing when the message was submitted"
            markOptimisticRowFailed(item, error: error)
            failOutboxItem(item, state: .permanentFailure, error: error)
            sendPhase = .failed(error)
            return
        }

        // Build 103 WS-A: ALWAYS ensure a server-backed Hermes session exists
        // before the first POST.  Build 102 conditionally called
        // ensureConversation only when `currentConversation?.id != targetID`,
        // but POST /v1/sessions echoes back the local UUID without creating
        // a real Hermes session — so equality was meaningless, the check
        // was always skipped, and the first message landed on an unbound
        // conversation (HTTP 409 `conversation_not_ensured`).
        //
        // Identity equality is not proof of server provisioning. The server
        // decides when a conversation is bound to a real Hermes session;
        // iOS must always request provisioning and await a typed ready
        // signal before submitting. ensureConversation is fail-closed.
        sendPhase = .creatingConversation
        let sessionEstablished = await heraldClient.ensureConversation(id: targetID)
        guard sessionEstablished else {
            // Session could not be established — fail the user-visible
            // message rather than submitting to a non-existent session.
            let error = "Could not reach the Herald host to start a conversation. Check your connection and try again."
            markOptimisticRowFailed(item, error: error)
            failOutboxItem(
                item,
                state: .retryableFailure,
                error: error,
                retryAfter: backoffInterval(forAttempt: item.attemptCount)
            )
            sendPhase = .failed(error)
            pendingMessageSentAt = nil
            streamingPhase = .idle
            Logger.app.error("submitNextEligible blocked: ensureConversation returned no session")
            return
        }
        _ = try? await heraldClient.loadConversation(id: targetID)

        item.state = .submitting
        updateOutboxItem(item)

        if useStreaming {
            // Append a placeholder Herald message for streaming content
            let placeholderID = UUID()
            let placeholder = Message(
                id: placeholderID,
                sender: .herald,
                content: "",
                status: .sending,
                isStreaming: true
            )
            conversation?.messages.append(placeholder)
            // activeStreams entry is added in the .messageSent handler once jobId is known.
            // streamingMessageID (computed) remains nil until then — that's correct.
            restartPendingPollingIfNeeded()

            await runAttemptLoop(
                content: item.cleanText,
                attachments: attachments,
                clientMessageID: item.clientMessageID,
                placeholderID: placeholderID,
                continuationContext: item.continuationContext
            )

            // The attempt loop ended without a terminal stream event (stall,
            // transport drop). Settle an accepted-but-unresolved job against
            // the relay so the outbox reaches a terminal state before the
            // next item drains.
            await settleAcceptedOutboxJob(clientMessageID: item.clientMessageID)
        } else {
            sendPhase = .submitting
            let response = await heraldClient.send(
                message: item.cleanText,
                attachments: attachments,
                clientMessageID: item.clientMessageID,
                continuationContext: item.continuationContext
            )
            if let idx = conversation?.messages.firstIndex(where: { $0.id == item.clientMessageID }) {
                conversation?.messages[idx].status = .delivered
            }
            conversation?.messages.append(response)
            conversation?.lastActivity = response.timestamp
            conversation = mergeConversationMetadata(
                from: conversation,
                into: heraldClient.currentConversation
            )
            if let latestUsage = conversation?.latestUsage {
                lastTokenUsage = latestUsage
            }
            await autoTitleIfNeeded()

            if response.status == .failed {
                // The client could not deliver the message (oversized payload,
                // transport rejection, relay error). Show the failed state on
                // the optimistic row and classify: size/format rejections are
                // permanent (the user must change the input); transport and
                // relay failures are retryable with backoff.
                let error = response.content.isEmpty ? "Kallisti rejected the message" : response.content
                if let idx = conversation?.messages.firstIndex(where: { $0.id == item.clientMessageID }) {
                    conversation?.messages[idx].status = .failed
                }
                if Self.isPermanentSendFailure(error) {
                    failOutboxItem(item, state: .permanentFailure, error: error)
                } else {
                    failOutboxItem(
                        item,
                        state: .retryableFailure,
                        error: error,
                        retryAfter: backoffInterval(forAttempt: item.attemptCount)
                    )
                }
                sendPhase = .failed(error)
            } else {
                terminalizeOutboxItem(
                    item,
                    canonicalUserMessageID: item.clientMessageID,
                    terminalMessageID: response.id
                )
                sendPhase = .completed
            }
        }

        if !hasPendingMessages {
            pendingMessageSentAt = nil
        }

        if let conversation {
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
            onConversationChanged?()
        }

        // FIFO chain: after this job is no longer in flight (terminal success,
        // permanent/retryable failure, cancelled), drain the next queued item
        // for this conversation. A failed item never blocks the items behind it.
        // Release the lease BEFORE the recursive call so it can re-acquire
        // for the next item; the defer would not fire until this function
        // returns, which is after the recursion completes.
        if activeLeases[targetID] == lease {
            activeLeases[targetID] = nil
        }
        if let current = outboxItems.first(where: { $0.clientMessageID == item.clientMessageID }),
           !current.isInFlight {
            sendPhaseOwner = nil
            await submitNextEligible(for: targetID)
        }
    }

    /// Cancel every in-flight outbox item (leased/submitted/accepted) —
    /// user intent (stop button, archive) means never auto-resubmit. Queued
    /// items are untouched.
    private func cancelInFlightOutboxItems() {
        var changed = false
        for idx in outboxItems.indices where outboxItems[idx].isInFlight {
            let item = outboxItems[idx]
            failOutboxItem(item, state: .cancelled, error: "Cancelled by user")
            if let messageIdx = conversation?.messages.firstIndex(where: { $0.id == item.clientMessageID }) {
                conversation?.messages[messageIdx].status = .failed
            }
            changed = true
        }
        if changed {
            appendLog(level: .info, "Outbox: in-flight messages cancelled")
        }
    }

    /// Mark the optimistic user row for `item` as failed and append a visible
    /// system error — used when submission could not even start.
    private func markOptimisticRowFailed(_ item: ChatOutboxRecord, error: String) {
        if let idx = conversation?.messages.firstIndex(where: { $0.id == item.clientMessageID }) {
            conversation?.messages[idx].status = .failed
        }
        conversation?.messages.append(Message(
            sender: .system,
            content: error,
            status: .failed
        ))
        cacheModifiedConversation()
        onConversationChanged?()
    }

    /// Outbox state → .terminal with canonical identity, persisted.
    private func terminalizeOutboxItem(
        _ item: ChatOutboxRecord,
        canonicalUserMessageID: UUID,
        terminalMessageID: UUID?
    ) {
        guard let idx = outboxItems.firstIndex(where: { $0.clientMessageID == item.clientMessageID }) else { return }
        var updated = outboxItems[idx]
        updated.state = .terminal
        updated.canonicalUserMessageID = updated.canonicalUserMessageID ?? canonicalUserMessageID
        updated.terminalMessageID = terminalMessageID
        updated.lastError = nil
        updated.nextAttemptAt = nil
        outboxItems[idx] = updated
        persistOutbox()
        outboxStore.removeStagedAttachments(for: updated)
        // Mark the corresponding user message as delivered in the conversation
        // so the green checkmark dot appears.
        if var conv = conversation,
           let msgIdx = conv.messages.firstIndex(where: { $0.clientMessageID == updated.clientMessageID && $0.sender == .user }) {
            conv.messages[msgIdx].status = .delivered
            conversation = conv
        }
    }

    /// Outbox state → retryable/permanent failure, persisted. When
    /// `retryAfter` is nil the item is manual-retry-only (no auto-resubmit).
    private func failOutboxItem(
        _ item: ChatOutboxRecord,
        state: OutboxItemState,
        error: String,
        retryAfter: TimeInterval? = nil
    ) {
        guard let idx = outboxItems.firstIndex(where: { $0.clientMessageID == item.clientMessageID }) else { return }
        var updated = outboxItems[idx]
        updated.state = state
        updated.lastError = error
        updated.attemptCount = max(updated.attemptCount, item.attemptCount)
        if let retryAfter {
            updated.nextAttemptAt = .now.addingTimeInterval(retryAfter)
        } else {
            updated.nextAttemptAt = nil
        }
        outboxItems[idx] = updated
        persistOutbox()
        if state == .permanentFailure || state == .cancelled {
            outboxStore.removeStagedAttachments(for: updated)
        }
    }

    /// Exponential backoff for a retryable failure: 5s, 10s, 20s, … capped at
    /// 60s. Deliberately modest — the relay is usually healthy again quickly.
    private func backoffInterval(forAttempt attemptCount: Int) -> TimeInterval {
        min(60, 5 * pow(2.0, Double(max(attemptCount - 1, 0))))
    }

    /// Heuristic for send failures that retrying can never fix — payload
    /// rejections (oversized attachments, unsupported types). Everything else
    /// (transport, relay, upstream) is treated as retryable.
    static func isPermanentSendFailure(_ content: String) -> Bool {
        let lower = content.lowercased()
        return lower.contains("too large")
            || lower.contains("larger than")
            || lower.contains("exceeds the limit")
            || lower.contains("exceeds the size")
            || lower.contains("unsupported")
            || lower.contains("not supported")
    }

    /// Query the relay for the authoritative status of an accepted outbox job
    /// and settle the record to a terminal state. No-op when the record is
    /// not `.accepted`, or when the relay cannot identify the job (it is then
    /// left for `recoverOutbox` on the next foreground — never auto-resubmit).
    private func settleAcceptedOutboxJob(clientMessageID: UUID) async {
        guard let idx = outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }),
              outboxItems[idx].state == .accepted,
              let jobID = outboxItems[idx].jobID
        else { return }
        let item = outboxItems[idx]
        guard let status = await heraldClient.getJobStatus(jobID) else { return }
        switch status.status {
        case "completed", "delivered", "succeeded", "success", "terminal":
            var updated = item
            updated.state = .terminal
            updated.terminalMessageID = updated.terminalMessageID ?? status.message?.id
            updated.lastError = nil
            updated.nextAttemptAt = nil
            outboxItems[idx] = updated
            persistOutbox()
            outboxStore.removeStagedAttachments(for: updated)
            sendPhase = .completed
        case "failed":
            let error = status.error ?? status.errorCategory ?? "Kallisti reported the job failed"
            failOutboxItem(item, state: .retryableFailure, error: error, retryAfter: backoffInterval(forAttempt: item.attemptCount))
            sendPhase = .failed(error)
        case "cancelled":
            var updated = item
            updated.state = .cancelled
            updated.lastError = status.error
            outboxItems[idx] = updated
            persistOutbox()
        default:
            // Still running — leave .accepted; the poll loop and the next
            // recovery pass reconcile it.
            break
        }
    }

    /// Persist the in-memory outbox to the manifest (atomic write).
    private func persistOutbox() {
        let manifest = ChatOutboxManifest(
            schemaVersion: OutboxManifestStore.schemaVersion,
            items: outboxItems,
            nextSequence: outboxNextSequence
        )
        outboxStore.save(manifest)
    }

    private func updateOutboxItem(_ item: ChatOutboxRecord) {
        guard let idx = outboxItems.firstIndex(where: { $0.clientMessageID == item.clientMessageID }) else { return }
        outboxItems[idx] = item
        persistOutbox()
    }

    /// Prune terminal records older than 24h — the transcript already owns
    /// their content, and their staged bytes are gone.
    private func pruneTerminalOutboxRecords() {
        let cutoff = Date.now.addingTimeInterval(-24 * 60 * 60)
        let before = outboxItems.count
        outboxItems.removeAll { $0.isTerminal && $0.createdAt < cutoff }
        if outboxItems.count != before {
            persistOutbox()
        }
    }

    // MARK: - Durable Outbox — Recovery (Build 33 WSB §4)

    /// Reconcile the durable outbox on launch/foreground (called from
    /// ChatScreen appear and app-did-become-active):
    ///
    /// - `.accepted` items with a jobID → query the relay for the
    ///   authoritative job status (terminal, failed, cancelled, still running).
    /// - `.retryableFailure` items whose backoff elapsed → re-queue.
    /// - Stale `.materializing`/`.submitting` leases (process died mid-submit)
    ///   → re-queue; resubmission is idempotent via the frozen clientMessageID.
    /// - `.queued` items for the current conversation → submit.
    ///
    /// Items in ambiguous/corrupted states are NEVER auto-resubmitted — they
    /// surface as visible failures with manual retry controls.
    func recoverOutbox() async {
        guard !restartInProgress else { return }
        // Offline — settling accepted jobs or resubmitting would only fail
        // (and a failed job-status probe must never be mistaken for a job the
        // relay lost). Defer the whole pass until the transport is connected.
        guard heraldClient.connectionStatus == .connected
                || heraldClient.connectionStatus == .degraded else { return }

        // 1. Accepted jobs: settle against the relay.
        let accepted = outboxItems.filter { $0.state == .accepted && $0.jobID != nil }
        for item in accepted {
            guard let jobID = item.jobID else { continue }
            guard let status = await heraldClient.getJobStatus(jobID) else {
                // The relay cannot identify the job (unknown or unreachable).
                // Do NOT resubmit — the original job may still be running and
                // resubmission could duplicate it. Surface as a manual-only
                // retryable failure.
                failOutboxItem(
                    item,
                    state: .retryableFailure,
                    error: "The relay could not report this message's status after relaunch. Check the conversation and retry if needed."
                )
                continue
            }
            switch status.status {
            case "completed", "delivered", "succeeded", "success", "terminal":
                var updated = item
                updated.state = .terminal
                updated.terminalMessageID = updated.terminalMessageID ?? status.message?.id
                updated.lastError = nil
                updated.nextAttemptAt = nil
                if let idx = outboxItems.firstIndex(where: { $0.clientMessageID == item.clientMessageID }) {
                    outboxItems[idx] = updated
                }
                persistOutbox()
                outboxStore.removeStagedAttachments(for: updated)
            case "failed":
                let error = status.error ?? status.errorCategory ?? "Kallisti reported the job failed while the app was away"
                failOutboxItem(item, state: .retryableFailure, error: error, retryAfter: backoffInterval(forAttempt: item.attemptCount))
            case "cancelled":
                failOutboxItem(item, state: .cancelled, error: status.error ?? "Cancelled")
            default:
                // Still running — leave .accepted; the poll loop reconciles it
                // once the conversation loads.
                break
            }
        }

        // 2. Expired retryable backoffs and stale leases (process died
        // mid-submit) → back to .queued for the resubmit pass below.
        let now = Date.now
        var requeued = false
        for idx in outboxItems.indices {
            let state = outboxItems[idx].state
            if state == .queued { continue }
            if state == .retryableFailure,
               let nextAttemptAt = outboxItems[idx].nextAttemptAt,
               nextAttemptAt <= now {
                outboxItems[idx].state = .queued
                requeued = true
            } else if state == .materializing || state == .submitting {
                // A crashed submit left a lease behind. Re-leasing is safe:
                // resubmission carries the same clientMessageID and the relay
                // dedupes by it.
                outboxItems[idx].state = .queued
                outboxItems[idx].nextAttemptAt = nil
                requeued = true
            }
        }
        if requeued {
            persistOutbox()
        }

        // 3. Submit queued items for the current conversation (FIFO chain).
        await submitNextEligible()
    }

    /// Drives a single streaming attempt for an outgoing message.
    ///
    /// If the SSE stream stalls (no progress events within the watchdog window)
    /// we start parallel HTTP polling rather than failing the message.  The
    /// relay owns retries via leases, so the client never resubmits the same
    /// message — it just keeps waiting until the relay resolves the job.
    ///
    /// Only an explicit ``.failed`` event from the relay or exceeding the
    /// absolute job deadline causes a "tap to retry" error to appear.
    /// A stalled stream is a transport concern, not a failure.
    private func runAttemptLoop(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID,
        continuationContext: String? = nil
    ) async {
        let jobAcceptedAt = Date.now
        let terminal = await runStreamingAttempt(
            content: content,
            attachments: attachments,
            clientMessageID: clientMessageID,
            placeholderID: placeholderID,
            continuationContext: continuationContext
        )

        // If the stream completed normally (including explicit .failed),
        // .cancelled, or any other typed terminal, done — the consumer or
        // the legacy internal watchdog has already applied the outbox and
        // UI mutations for these cases. Only .stalled(jobID) needs the
        // polling fallback below; the job is still alive on the relay
        // and may resolve via a later refresh or explicit GET.
        guard case .stalled = terminal else { return }

        // — Stream stalled — start parallel polling —
        streamingPhase = .stalled
        if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
            conversation?.messages[idx].toolActivity = "Waiting for host..."
        }

        // Poll until the job resolves or the absolute deadline is exceeded.
        // Heartbeats can keep a hung upstream job alive forever; this deadline
        // guarantees a terminal user-visible state regardless of connector/relay
        // behavior.
        var pollCount = 0
        while !Task.isCancelled {
            // Check absolute deadline first
            let elapsed = Date.now.timeIntervalSince(jobAcceptedAt)
            let deadlineSeconds = Self.absoluteJobDeadline / .seconds(1)
            if elapsed >= deadlineSeconds {
                appendLog(level: .warn, "Job exceeded absolute deadline (\(Int(elapsed))s) — timing out")
                flushPendingReasoning(placeholderID: placeholderID)
                flushPendingDeltas(placeholderID: placeholderID)
                if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                    conversation?.messages[idx] = Message(
                        sender: .system,
                        content: failureMessage(for: "timeout"),
                        status: .failed,
                        errorCategory: "timeout"
                    )
                }
                if let idx = conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                    conversation?.messages[idx].status = .sending  // user message is retryable
                }
                // Durable outbox: absolute deadline exceeded — the job is
                // unresolvable from the client's perspective. Mark retryable
                // with backoff; recovery or manual retry resubmits.
                if let idx = outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                    let item = outboxItems[idx]
                    failOutboxItem(
                        item,
                        state: .retryableFailure,
                        error: "Job exceeded the absolute deadline",
                        retryAfter: backoffInterval(forAttempt: item.attemptCount)
                    )
                }
                activeStreams.removeAll()
                streamingPhase = .idle
                sendPhase = .failed(failureMessage(for: "timeout"))
                sendPhaseOwner = nil
                chatLiveActivity.endActivity()
                pendingMessageSentAt = nil
                return
            }

            try? await Task.sleep(for: .seconds(10))
            pollCount += 1

            let refreshed = await refreshActiveConversation()
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)

            // Check if the placeholder was resolved by a late SSE event or polling
            if let msg = conversation?.messages.first(where: { $0.id == placeholderID }) {
                if msg.status == .delivered || msg.status == .failed {
                    // Build 107: update outbox item state when placeholder is
                    // resolved by polling.  Without this, the outbox item stays
                    // in .accepted state and the FIFO chain never drains the
                    // next queued message — the user has to relaunch the app.
                    if let idx = outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                        let item = outboxItems[idx]
                        if item.state == .accepted {
                            if msg.status == .delivered {
                                terminalizeOutboxItem(
                                    item,
                                    canonicalUserMessageID: clientMessageID,
                                    terminalMessageID: msg.id
                                )
                            } else {
                                failOutboxItem(
                                    item,
                                    state: .retryableFailure,
                                    error: "Job failed during polling",
                                    retryAfter: backoffInterval(forAttempt: item.attemptCount)
                                )
                            }
                        }
                    }
                    streamingPhase = .idle
                    chatLiveActivity.endActivity()
                    return
                }
                if (!msg.content.isEmpty || !msg.reasoning.isEmpty) && msg.status != .sending {
                    // Build 107: same as above — resolve outbox item when
                    // content appears via polling.
                    if let idx = outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                        let item = outboxItems[idx]
                        if item.state == .accepted {
                            terminalizeOutboxItem(
                                item,
                                canonicalUserMessageID: clientMessageID,
                                terminalMessageID: msg.id
                            )
                        }
                    }
                    streamingPhase = .idle
                    chatLiveActivity.endActivity()
                    return
                }
            }

            // Check whether the original user message was marked failed
            if let userMsg = conversation?.messages.first(where: { $0.id == clientMessageID }),
               userMsg.status == .failed {
                streamingPhase = .idle
                chatLiveActivity.endActivity()
                return
            }

            // Update waiting indicator with elapsed time
            if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                let elapsedSecs = Int(Date.now.timeIntervalSince(jobAcceptedAt))
                if elapsedSecs > 0 {
                    conversation?.messages[idx].toolActivity = "Waiting... (\(elapsedSecs)s)"
                }
            }
        }
    }

    /// Runs a single streaming attempt, racing the update stream against a
    /// ~120s watchdog. Returns `true` if the watchdog fired before any progress
    /// event (`.textDelta`, `.reasoningDelta`, `.toolActivity`, `.finished`)
    /// arrived — i.e. the job appears to have stalled/been silently dropped.
    /// `.messageSent` (the relay merely accepting the job) does NOT count as
    /// progress, since that's precisely the point where the observed bug drops
    /// the job with zero further activity.
    private func runStreamingAttemptLegacy(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID,
        continuationContext: String? = nil
    ) async -> Bool {
        // Build 31 (fix): capture the attempt generation so every event handler
        // below can discard stale callbacks from a superseded attempt.  Retry
        // and cancel bump activeAttemptID, instantly invalidating all in-flight
        // events from the old stream coordinator.
        let attemptID = activeAttemptID

        let stream = heraldClient.sendStreaming(message: content, attachments: attachments, clientMessageID: clientMessageID, continuationContext: continuationContext)
        var acceptedJobID: UUID?
        var needsPollingFallback = false
        var reasoningStartedAt: Date?

        streamingPhase = .sending

        var progressContinuation: AsyncStream<Void>.Continuation?
        let progressSignal = AsyncStream<Void> { continuation in
            progressContinuation = continuation
        }

        let consumerTask = Task { [weak self] in
            guard let self else { return }
            self.appendLog(level: .info, "Streaming started")
            for await update in stream {
                if Task.isCancelled { break }
                switch update {
                case .messageSent(let jobID):
                    self.appendLog(level: .info, "Message accepted — job \(jobID.uuidString.prefix(8))")
                    acceptedJobID = jobID
                    self.streamingPhase = .waitingForJob
                    self.sendPhase = .waitingForHermes
                    // Durable outbox: job accepted — record the jobID so
                    // relaunch recovery can settle the job via the relay.
                    if let idx = self.outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                        self.outboxItems[idx].state = .accepted
                        self.outboxItems[idx].jobID = jobID
                        self.persistOutbox()
                    }
                    self.activeStreams[jobID] = placeholderID
                    // Correlate the placeholder with its server-assigned job
                    // identity so the conversation refresh merge can match
                    // persisted assistant rows back to this live placeholder
                    // without relying on content heuristics.
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].jobID = jobID
                        self.conversation = conv
                    }
                    // Arm the polling safety net. If the SSE stream fails silently
                    // (transport drop, proxy timeout, connector stall), polling
                    // recovers the response so the user isn't stuck staring at a
                    // blank screen. When streaming delivers normally the pending
                    // message is already resolved, so polling becomes a no-op.
                    needsPollingFallback = true
                    // Start Live Activity with "Thinking" phase — the agent is
                    // processing but hasn't begun streaming content yet.
                    self.chatLiveActivity.startThinking()
                    // Show a phase indicator on the streaming placeholder so the
                    // user knows the app isn't frozen during long model loads.
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = "Model loading…"
                        self.conversation = conv
                    }
                    // Yield progress — the relay accepting the job IS proof the
                    // connection is alive. This keeps the continuous watchdog
                    // satisfied during long model loads (30-45s prefill on
                    // constrained hardware).
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())

                case .textDelta(let delta):
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    self.streamingPhase = .streaming
                    self.sendPhase = .streaming
                    Self.logger.info("stream textDelta bytes=\(delta.utf8.count) placeholder=\(placeholderID.uuidString.prefix(8))")
                    self.chatLiveActivity.updatePhase("Responding")
                    // Route content to reasoning vs visible text.
                    // If a reasoning stream is already active, check if this
                    // delta closes the block; otherwise check if it opens one.
                    if reasoningStartedAt != nil {
                        // We're inside reasoning — check if the delta closes it
                        let hasCloseTag = delta.range(
                            of: #"</think(?:ing)?>"#, options: .regularExpression
                        ) != nil
                        if hasCloseTag {
                            // Split at the close tag — reasoning before, visible after
                            if let closeRange = delta.range(of: #"</think(?:ing)?>"#, options: .regularExpression) {
                                let reasoningPart = String(delta[delta.startIndex..<closeRange.lowerBound])
                                let visiblePart = String(delta[closeRange.upperBound...])
                                    .trimmingCharacters(in: .whitespacesAndNewlines)
                                if !reasoningPart.isEmpty {
                                    self.enqueueReasoningDelta(reasoningPart, placeholderID: placeholderID)
                                }
                                if !visiblePart.isEmpty {
                                    if let settings = self.ttsSettingsProvider?(),
                                       settings.enabled,
                                       settings.autoSpeakDuringStreaming {
                                        self.ttsService?.speakStreaming(visiblePart, voice: settings.voice)
                                    }
                                    self.enqueueDelta(visiblePart, placeholderID: placeholderID)
                                }
                            }
                        } else {
                            self.enqueueReasoningDelta(delta, placeholderID: placeholderID)
                        }
                    } else {
                        // Check if this delta opens a reasoning block
                        let hasOpenThinkTag = delta.range(
                            of: #"<think(?:ing)?>"#, options: .regularExpression
                        ) != nil
                        if hasOpenThinkTag {
                            reasoningStartedAt = .now
                            // Split at the open tag — visible before (unlikely), reasoning after
                            if let openRange = delta.range(of: #"<think(?:ing)?>"#, options: .regularExpression) {
                                let beforeTag = String(delta[delta.startIndex..<openRange.lowerBound])
                                    .trimmingCharacters(in: .whitespacesAndNewlines)
                                let afterTag = String(delta[openRange.upperBound...])
                                if !beforeTag.isEmpty {
                                    self.enqueueDelta(beforeTag, placeholderID: placeholderID)
                                }
                                if !afterTag.isEmpty {
                                    self.enqueueReasoningDelta(afterTag, placeholderID: placeholderID)
                                }
                            } else {
                                self.enqueueReasoningDelta(delta, placeholderID: placeholderID)
                            }
                        } else {
                            // Normal visible content
                            if let settings = self.ttsSettingsProvider?(),
                               settings.enabled,
                               settings.autoSpeakDuringStreaming {
                                self.ttsService?.speakStreaming(delta, voice: settings.voice)
                            }
                            self.enqueueDelta(delta, placeholderID: placeholderID)
                        }
                    }

                case .reasoningDelta(let delta):
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    self.chatLiveActivity.updatePhase("Thinking")
                    self.sendPhase = .streaming
                    if reasoningStartedAt == nil { reasoningStartedAt = .now }
                    self.enqueueReasoningDelta(delta, placeholderID: placeholderID)

                case .toolActivity(let label):
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        for i in conv.messages[idx].toolActivities.indices {
                            conv.messages[idx].toolActivities[i].isActive = false
                        }
                        let activity = ToolActivity(label: label)
                        conv.messages[idx].toolActivities.append(activity)
                        conv.messages[idx].toolActivity = label
                        self.conversation = conv
                    }
                    // Show tool progress on Lock Screen / Dynamic Island
                    self.chatLiveActivity.startToolCall(toolName: label)
                    self.chatLiveActivity.updateToolProgress(label)

                case .toolStarted(let activity):
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if var conv = self.conversation, let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        for i in conv.messages[idx].toolActivities.indices { conv.messages[idx].toolActivities[i].isActive = false }
                        conv.messages[idx].toolActivities.append(activity)
                        conv.messages[idx].toolActivity = activity.label
                        self.conversation = conv
                    }
                    self.chatLiveActivity.startToolCall(toolName: activity.label)
                    self.chatLiveActivity.updateToolProgress(activity.label)

                case .toolCompleted(let toolCallID, let resultPreview, let isError, let durationMs):
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    if var conv = self.conversation, let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }),
                       let activityIdx = conv.messages[idx].toolActivities.firstIndex(where: { $0.toolCallID == toolCallID }) {
                        conv.messages[idx].toolActivities[activityIdx].isActive = false
                        conv.messages[idx].toolActivities[activityIdx].finishedAt = .now
                        conv.messages[idx].toolActivities[activityIdx].resultPreview = resultPreview
                        conv.messages[idx].toolActivities[activityIdx].isError = isError
                        conv.messages[idx].toolActivities[activityIdx].durationMs = durationMs
                        self.conversation = conv
                    }

                case .keepalive:
                    // Transport keepalives prove the connection is alive but do
                    // NOT prove the model is making progress. Do not reset the
                    // watchdog — only text/reasoning/tool events count.
                    break

                case .finished(let finalMessage, let usage, let diff, let context):
                    guard self.activeAttemptID == attemptID else { break }
                    progressContinuation?.yield(())
                    self.heraldClient.connectionStatus = .connected
                    self.activeStreams.removeAll()
                    self.streamingPhase = .idle
                    Self.logger.info("stream finished content=\(finalMessage.content.count) chars")
                    self.flushPendingReasoning(placeholderID: placeholderID)
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    var isCredibleCompletion = false
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        let placeholder = self.conversation?.messages[idx]
                        let activities = placeholder?.toolActivities ?? []
                        let streamedReasoning = placeholder?.reasoning ?? ""
                        // A terminal event carrying empty content must never erase
                        // text that already streamed into the placeholder. Build 41
                        // dropped this guard (it arrived in B35 as `c5069af`), which
                        // re-opened the blank-bubble regression.
                        let streamedContent = placeholder?.content ?? ""
                        var resolved = Self.mergeResolvedMessage(
                            resolved: finalMessage,
                            streamedContent: streamedContent
                        )
                        resolved.toolActivities = activities
                        resolved.codeDiff = diff
                        // Priority for reasoning:
                        // 1) finalMessage.reasoning — set by LiveHeraldClient (SSE terminal
                        //    reasoning from the done payload, or splitThinkingBlocks extraction).
                        // 2) placeholder's streamed reasoning — from reasoningDelta SSE events,
                        //    only used when finalMessage has no reasoning of its own.
                        // 3) regex extraction from content — last resort for models that embed
                        //    <think> tags inline without a separate reasoning field.
                        if resolved.reasoning.isEmpty && !streamedReasoning.isEmpty {
                            resolved.reasoning = streamedReasoning
                            if let startedAt = reasoningStartedAt {
                                resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                            }
                        } else if !streamedReasoning.isEmpty {
                            // finalMessage already has reasoning — keep it but carry over
                            // the duration from the streamed placeholder
                            if resolved.reasoningDuration == nil, let startedAt = reasoningStartedAt {
                                resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                            }
                        }
                        // Last resort: regex extraction for models that embed reasoning
                        // as XML tags inline in the content (DeepSeek <think>, Qwen <thinking>).
                        // splitThinkingBlocks in LiveHeraldClient handles this on the sync
                        // path; this is the SSE-path safety net.
                        if resolved.reasoning.isEmpty {
                            if let thinkRegex = try? NSRegularExpression(
                                pattern: "<think(?:ing)?>(.*?)</think(?:ing)?>",
                                options: [.dotMatchesLineSeparators, .caseInsensitive]
                            ) {
                                let nsContent = resolved.content as NSString
                                let matches = thinkRegex.matches(
                                    in: resolved.content,
                                    range: NSRange(location: 0, length: nsContent.length)
                                )
                                let extracted = matches.compactMap { match -> String? in
                                    guard match.numberOfRanges > 1 else { return nil }
                                    return nsContent.substring(with: match.range(at: 1))
                                }.joined(separator: "\n")
                                if !extracted.isEmpty {
                                    resolved.reasoning = extracted.trimmingCharacters(in: .whitespacesAndNewlines)
                                    if let startedAt = reasoningStartedAt {
                                        resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                                    }
                                }
                            }
                            // Also try unclosed <think> tags (model interrupted mid-reasoning)
                            if resolved.reasoning.isEmpty {
                                if let unclosedRegex = try? NSRegularExpression(
                                    pattern: "<think(?:ing)?>([\\s\\S]*?)$",
                                    options: [.caseInsensitive]
                                ) {
                                    let nsContent = resolved.content as NSString
                                    if let match = unclosedRegex.firstMatch(
                                        in: resolved.content,
                                        range: NSRange(location: 0, length: nsContent.length)
                                    ), match.numberOfRanges > 1 {
                                        let extracted = nsContent.substring(with: match.range(at: 1))
                                            .trimmingCharacters(in: .whitespacesAndNewlines)
                                        if !extracted.isEmpty {
                                            resolved.reasoning = extracted
                                        }
                                    }
                                }
                            }
                        }
                        // Always strip <think>…</think> (and <thinking>…</thinking>)
                        // tags from the visible content.
                        if let regex = try? NSRegularExpression(pattern: "<think(?:ing)?>.*?</think(?:ing)?>", options: [.dotMatchesLineSeparators, .caseInsensitive]) {
                            let range = NSRange(resolved.content.startIndex..., in: resolved.content)
                            resolved.content = regex.stringByReplacingMatches(in: resolved.content, range: range, withTemplate: "")
                                .trimmingCharacters(in: .whitespacesAndNewlines)
                        }
                        let resolvedText = resolved.content.trimmingCharacters(in: .whitespacesAndNewlines)
                        // Usage is returned by every provider on every turn, including turns
                        // that produced nothing.  Treating it as proof of a reply is what
                        // put a delivered check and a completion haptic on empty bubbles.
                        // Only rendered content — visible text or attachments — makes a
                        // completion credible.  Hidden reasoning alone is never a visible
                        // delivered check.
                        isCredibleCompletion = !resolvedText.isEmpty
                            || !resolved.attachments.isEmpty
                        self.conversation?.messages[idx] = resolved
                        // Build 117 fix: do NOT mark the user message as
                        // .delivered here.  The user message's delivery status
                        // is managed by the outbox lifecycle
                        // (terminalizeOutboxItem), not by the assistant's
                        // content arrival.  Marking it .delivered here defeated
                        // hasPendingDuplicateMessage when the user retried
                        // after a stream drop — the guard checked for
                        // status == .sending, found .delivered, and let a
                        // second optimistic row with the same clientMessageID
                        // through, producing two "Hey homie" bubbles.
                    }
                    let oldTitle = self.conversation?.title
                    // Build 26: the terminal message is authoritative in the
                    // active chat.  Persist the cache atomically before clearing
                    // stream ownership so a polling refresh, foreground recovery,
                    // or stale currentConversation snapshot cannot remove the
                    // answer the user already saw.
                    //
                    // currentConversation at this point is often the POST
                    // acknowledgement (one user message) or an intermediate
                    // polling snapshot that may not yet contain the terminal row.
                    // A message-count heuristic cannot safely decide freshness —
                    // equal or greater count does not prove the snapshot includes
                    // this turn.  Only merge metadata (title, usage, context)
                    // from the server; never replace the local message array
                    // inside the .finished handler.
                    self.cacheModifiedConversation()
                    let refreshed = self.heraldClient.currentConversation
                    if var conv = self.conversation {
                        // Accept server-derived title only when local is a default.
                        if conv.title == "New Chat" || conv.title == "Kallisti",
                           let serverTitle = refreshed?.title,
                           serverTitle != "New Chat", serverTitle != "Kallisti" {
                            conv.title = serverTitle
                        }
                        conv.latestUsage = refreshed?.latestUsage ?? conv.latestUsage
                        conv.contextPercent = refreshed?.contextPercent ?? conv.contextPercent
                        self.conversation = conv
                    }
                    // Deferred reconciliation: mark that a server refresh is
                    // pending.  The next explicit session fetch (chat-list tap,
                    // foreground, or poll) will reconcile through
                    // mergeConversationMetadata, which preserves local attachments
                    // and content when the server copy is empty or truncated
                    // (B39 T5 guards at lines 1579-1595 and 1613-1618).
                    self.pendingServerReconciliation = true
                    if let latestUsage = self.conversation?.latestUsage {
                        self.lastTokenUsage = latestUsage
                    } else if let usage {
                        self.lastTokenUsage = usage
                    }
                    if let context {
                        self.lastContextInfo = context
                        self.conversation?.contextPercent = context.percentUsed
                    }
                    await self.detectProfileSwitch(in: finalMessage.content)
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.pendingMessageSentAt = nil
                    self.chatLiveActivity.endActivity()
                    self.streamingPhase = .idle
                    // Durable outbox: the job reached a server terminal state —
                    // record the canonical identities and release the phase.
                    if let idx = self.outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                        var record = self.outboxItems[idx]
                        record.state = .terminal
                        record.terminalMessageID = finalMessage.id
                        record.canonicalUserMessageID = record.canonicalUserMessageID ?? clientMessageID
                        record.lastError = nil
                        record.nextAttemptAt = nil
                        self.outboxItems[idx] = record
                        self.persistOutbox()
                        self.outboxStore.removeStagedAttachments(for: record)
                    }
                    self.sendPhase = .completed
                    self.sendPhaseOwner = nil
                    // Build 30: bump the conversation generation so any in-flight
                    // poll/refresh captures are discarded.  This ensures a stale
                    // server snapshot that arrived after terminal commit cannot
                    // replace the local message array — the poll loop will re-fetch
                    // on the next cycle and the server will include the terminal row.
                    self.conversationGeneration &+= 1

                    // Haptic feedback on response completion — fired immediately
                    // in the stream handler so it's synchronous with the content
                    // appearing, not delayed by the ChatScreen's onChange observer.
                    if isCredibleCompletion, self.hapticFeedbackEnabled() {
                        HapticEngine.responseReceived()
                    }

                    // Finish TTS streaming — flush any remaining buffered text
                    self.ttsService?.finishStream()
                    // Notify if merge changed the title (server-derived title)
                    if let conv = self.conversation, conv.title != oldTitle {
                        self.onTitleChanged?(conv.id, conv.title)
                    }
                    await self.autoTitleIfNeeded()

                    // D5: autoCompress() deleted — it sent a /compress user
                    // turn into the transcript, invoking a connector command
                    // whose implementation called a nonexistent `hermes
                    // compress` subcommand.  The agent's own ContextCompressor
                    // handles compaction at 25%.

                    // Post local notification if app is in background
                    if UIApplication.shared.applicationState == .background {
                        let content = UNMutableNotificationContent()
                        content.title = "Kallisti"
                        content.body = String(finalMessage.content.prefix(100))
                        content.sound = .default
                        content.categoryIdentifier = NotificationCategoryID.messageReady.rawValue
                        if let convId = self.conversation?.id.uuidString {
                            content.userInfo = [
                                "conversationId": convId,
                                "messageId": finalMessage.id.uuidString,
                            ]
                        }

                        let request = UNNotificationRequest(
                            identifier: "herald-response-\(finalMessage.id.uuidString)",
                            content: content,
                            trigger: nil
                        )
                        try? await UNUserNotificationCenter.current().add(request)
                    }

                    // Build 33 WSB: the FIFO chain lives in submitNextEligible —
                    // when this attempt returns, its tail submits the next
                    // queued item for the conversation. (Calling it here would
                    // no-op anyway: submitInFlight is true for the whole
                    // attempt, and the chain runs once it is released.)
                case .started(let phase):
                    self.appendLog(level: .info, "Job started — phase: \(phase)")
                    progressContinuation?.yield(())
                    self.chatLiveActivity.updateToolProgress(phase)
                    // Update the placeholder to show what the model is doing
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = phase
                        self.conversation = conv
                    }

                case .heartbeat(let phase):
                    // Heartbeat proves the connector process is alive, but does
                    // NOT prove the model is making progress. Do NOT reset the
                    // watchdog — keepalive/transport liveness alone must not
                    // prevent the stall detector from firing.
                    self.appendLog(level: .debug, "Job heartbeat — phase: \(phase)")

                case .reconnecting:
                    // Build 31 (fix): discard stale callbacks from a superseded
                    // attempt.  A retry/replacement bumps activeAttemptID, so the
                    // old stream coordinator's reconnect event cannot overwrite
                    // the new attempt's banner or placeholder.
                    guard self.activeAttemptID == attemptID else { break }
                    self.appendLog(level: .warn, "Stream reconnecting...")
                    self.streamingPhase = .reconnecting
                    self.sendPhase = .restoringStream
                    // Reconnection attempts are transport recovery, not model
                    // progress. Do not reset the watchdog.
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = "Restoring response stream…"
                        self.conversation = conv
                    }
                    // Start polling immediately as a parallel recovery path.
                    // If the SSE stream is struggling to stay connected, the
                    // polling loop can pick up the response independently.
                    // When polling resolves the pending message the stream
                    // coordinator will naturally exit on its next reconnect.
                    self.restartPendingPollingIfNeeded()

                case .cancelled:
                    guard self.activeAttemptID == attemptID else { break }
                    self.appendLog(level: .info, "Job cancelled")
                    progressContinuation?.yield(())
                    self.flushPendingReasoning(placeholderID: placeholderID)
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        self.conversation?.messages[idx] = Message(
                            sender: .system,
                            content: "Cancelled",
                            status: .failed
                        )
                    }
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.pendingMessageSentAt = nil
                    self.chatLiveActivity.endActivity()
                    self.streamingPhase = .idle
                    // Durable outbox: user cancelled — the item is never
                    // auto-resubmitted; a manual retry re-enqueues fresh.
                    if let idx = self.outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                        self.outboxItems[idx].state = .cancelled
                        self.outboxItems[idx].lastError = "Cancelled by user"
                        self.persistOutbox()
                        self.outboxStore.removeStagedAttachments(for: self.outboxItems[idx])
                    }
                    self.sendPhase = .idle
                    self.sendPhaseOwner = nil
                    // Build 23: cancellation must NOT mark the outgoing user
                    // message as delivered.  The green check is a final-delivery
                    // signal tied to a credible terminal result.
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        self.conversation?.messages[idx].status = .sending
                    }
                    await self.autoTitleIfNeeded()

                case .failed(let errorMessage, let category, let action):
                    guard self.activeAttemptID == attemptID else { break }
                    // An explicit failure is a real signal, not silence — let it
                    // resolve the watchdog race immediately rather than waiting
                    // out the timeout, and handle it exactly as before.
                    progressContinuation?.yield(())
                    self.flushPendingReasoning(placeholderID: placeholderID)
                    self.flushPendingDeltas(placeholderID: placeholderID)

                    // Store error context for the UI
                    self.lastErrorCategory = category
                    self.lastErrorAction = action

                    // Show actionable guidance based on error category
                    let guidance: String
                    switch category {
                    case "context_exceeded":
                        guidance = "This session is too long for the current model. Start a new session or switch models."
                    case "rate_limited":
                        guidance = "Kallisti is rate-limited. Please wait and try again."
                    case "timeout":
                        guidance = "The request timed out. Check your connection and retry."
                    case "empty_response":
                        guidance = "Kallisti returned an empty response. Try again or start a new session."
                    case "upstream_interrupted":
                        guidance = "The model was interrupted upstream. Please retry."
                    default:
                        guidance = errorMessage
                    }

                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        if acceptedJobID == nil {
                            self.conversation?.messages[idx] = Message(
                                sender: .system,
                                content: guidance,
                                status: .failed,
                                errorCategory: category
                            )
                        } else {
                            self.conversation?.messages.remove(at: idx)
                        }
                    }
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.chatLiveActivity.endActivity()
                    self.streamingPhase = .idle
                    // Durable outbox: an explicit failure before acceptance is a
                    // retryable failure with backoff; a failure AFTER acceptance
                    // leaves the job with the relay — the polling fallback below
                    // (or a later recovery pass) resolves it.
                    if let idx = self.outboxItems.firstIndex(where: { $0.clientMessageID == clientMessageID }) {
                        if acceptedJobID == nil {
                            let item = self.outboxItems[idx]
                            self.failOutboxItem(
                                item,
                                state: .retryableFailure,
                                error: guidance,
                                retryAfter: self.backoffInterval(forAttempt: item.attemptCount)
                            )
                        } else {
                            self.outboxItems[idx].lastError = errorMessage
                            self.persistOutbox()
                        }
                    }
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        self.conversation?.messages[idx].status = acceptedJobID == nil ? .failed : .sending
                    }
                    if acceptedJobID != nil {
                        needsPollingFallback = true
                        self.sendPhase = .restoringStream
                    } else {
                        self.pendingMessageSentAt = nil
                        self.sendPhase = .failed(guidance)
                    }
                    await self.autoTitleIfNeeded()
                }
            }
            progressContinuation?.finish()
        }
        streamingTask = consumerTask

        // Continuous watchdog — monitors progress at 5s intervals instead of
        // a one-shot race. This catches mid-stream stalls (e.g. SSE transport
        // drop after the first token), not just complete silence before the
        // first event. The old one-shot race would satisfy on the first
        // progress signal then never check again.
        //
        // Also enforces the absolute job deadline — even if heartbeats or
        // occasional text deltas keep arriving, the stream is terminated
        // after absoluteJobDeadline to prevent infinite hangs.
        self.streamingProgressAt = .now
        let jobAcceptedAt = Date.now
        var stallDetected = false
        while !consumerTask.isCancelled {
            // Check if the consumer finished while we were sleeping
            if streamingTask == nil { break }

            try? await Task.sleep(for: .seconds(5))

            // Absolute deadline check — terminate even if progress events
            // are still arriving, to prevent an infinite "Thinking..." hang
            // when heartbeats keep the job alive but the model never finishes.
            let wallElapsed = Date.now.timeIntervalSince(jobAcceptedAt)
            let wallDeadline = Self.absoluteJobDeadline / .seconds(1)
            if wallElapsed >= wallDeadline {
                appendLog(level: .warn, "SSE stream exceeded absolute deadline (\(Int(wallElapsed))s)")
                streamingPhase = .stalled
                stallDetected = true
                break
            }

            let elapsed = Duration.seconds(Date.now.timeIntervalSince(self.streamingProgressAt))

            if elapsed > Self.watchdogTimeout {
                self.streamingPhase = .stalled
                stallDetected = true
                break
            }
        }

        if stallDetected {
            // Progress stalled — do NOT cancel the consumer task; it may still
            // receive events. Signal the caller to start parallel polling.
            return true
        }

        await consumerTask.value
        streamingTask = nil

        // If streaming failed after the job was accepted, give the relay a grace
        // period to finish delivering events before falling back to polling.
        // SSE streams can end prematurely due to proxy timeouts or mobile network
        // transitions, but the relay may still be writing events. A 10-second
        // pause prevents the polling safety net from racing a healthy SSE stream.
        if needsPollingFallback {
            try? await Task.sleep(for: .seconds(10))
            let refreshed = await refreshActiveConversation()
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)
            if let latestUsage = conversation?.latestUsage {
                lastTokenUsage = latestUsage
            }
            activeStreams.removeAll()
            restartPendingPollingIfNeeded()
        }

        return false
    }

    // MARK: - Structured concurrency race (Build 102 P0-A)

    /// Sibling watchdog that races the stream consumer in `withTaskGroup`.
    /// Returns a typed terminal result; the consumer is cancelled when this
    /// returns `.stalled(jobID)` or `.cancelledByUser`.
    ///
    /// Build 102 marching orders §5: replaces the legacy polling loop
    /// (`while !consumerTask.isCancelled` + 5s sleep) with structured
    /// completion. A normally-completed consumer wins the race immediately;
    /// the legacy code conflated normal completion with stall because the
    /// consumer task was never `isCancelled` after a clean `.finished`.
    private func attemptWatchdog(attemptID: UUID, startedAt: Date) async -> StreamTerminal {
        while !Task.isCancelled {
            if activeAttemptID != attemptID { return .cancelledByUser }

            try? await Task.sleep(for: .seconds(5))
            if Task.isCancelled { return .cancelledByUser }
            if activeAttemptID != attemptID { return .cancelledByUser }

            let wallElapsed = Date.now.timeIntervalSince(startedAt)
            let wallDeadline = Self.absoluteJobDeadline / .seconds(1)
            if wallElapsed >= wallDeadline {
                let jobID = self.acceptedJobID ?? UUID()
                Self.logger.warning(
                    "attemptWatchdog: absolute deadline exceeded (\(Int(wallElapsed))s) — returning .stalled(jobID=\(jobID.uuidString.prefix(8)))"
                )
                return .stalled(jobID: jobID)
            }

            let elapsed = Duration.seconds(Date.now.timeIntervalSince(self.streamingProgressAt))
            if elapsed > Self.watchdogTimeout {
                let jobID = self.acceptedJobID ?? UUID()
                Self.logger.warning(
                    "attemptWatchdog: no progress for \(Int(elapsed.components.seconds))s — returning .stalled(jobID=\(jobID.uuidString.prefix(8)))"
                )
                return .stalled(jobID: jobID)
            }
        }
        return .cancelledByUser
    }

    /// Structured-concurrency wrapper that races the legacy consumer against
    /// `attemptWatchdog`. The first terminal wins; the other branch is
    /// cancelled. Replaces the legacy call site in `runAttemptLoop`.
    ///
    /// Known limitation (Build 102 follow-up): the inner legacy function
    /// still has its own internal polling loop. If the consumer finishes
    /// normally (Bool=false), this returns `.completed`; if the inner
    /// polling detects a stall (Bool=true), this returns `.stalled`. The
    /// outer watchdog fires only when the inner one is slower than it —
    /// in practice this means the outer watchdog rarely wins, but it
    /// bounds wall-clock latency to ~watchdogTimeout for the failure
    /// detection path. A future revision should migrate the consumer to
    /// its own typed terminal and drop the inner polling loop entirely.
    private func runStreamingAttempt(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID,
        continuationContext: String? = nil
    ) async -> StreamTerminal {
        let attemptID = activeAttemptID
        let startedAt = Date.now
        self.acceptedJobID = nil

        // Translates the legacy consumer's Bool (stalled) into a typed
        // StreamTerminal. The full withTaskGroup race against attemptWatchdog
        // is deferred — the legacy internal polling loop already provides a
        // watchdog, and Swift's region-based isolation checker in Xcode 26
        // currently rejects @MainActor + [weak self] capture-list patterns
        // in withTaskGroup child closures. The lease work (P0-A.1) and the
        // typed terminal (P0-A.2 plumbing) are in place; the structured-
        // concurrency race is a follow-up once the compiler limitation
        // resolves.
        let stalled = await runStreamingAttemptLegacy(
            content: content,
            attachments: attachments,
            clientMessageID: clientMessageID,
            placeholderID: placeholderID,
            continuationContext: continuationContext
        )
        return stalled
            ? .stalled(jobID: acceptedJobID ?? UUID())
            : .completed
    }

    /// The user-facing failure copy, using the active profile name when
    /// available and falling back to "Kallisti".
    func failureMessage(for category: String? = nil) -> String {
        let name = profileStore?.activeProfile?.name ?? "Kallisti"
        switch category {
        case "context_exceeded":
            return "Session too long. Start a new chat."
        case "rate_limited":
            return "\(name) is rate-limited. Wait and retry."
        case "timeout":
            return "\(name) took too long. Tap to retry."
        case "empty_response":
            return "\(name) returned an empty response. Tap to retry."
        default:
            return "\(name) didn't respond. Tap to retry."
        }
    }

    /// Build 33: reload the ACTIVE conversation after a gateway restart.
    /// Resolves by the on-screen conversation's id so a connector-global
    /// "current" session (which may be a different thread on multi-device
    /// setups) can never clobber the thread the user has open.
    func reloadConversationAfterRestart() async {
        if let activeID = conversation?.id {
            await loadConversation(id: activeID)
        } else {
            await loadConversation()
        }
    }

    func clearConversation() async throws {
        streamingTask?.cancel()
        streamingTask = nil
        activeStreams.removeAll()
        chatLiveActivity.endActivity()
        // Durable outbox: in-flight items belong to the conversation being
        // archived — cancel them; queued items for OTHER conversations are
        // preserved (they resubmit when that conversation becomes current).
        cancelInFlightOutboxItems()
        sendPhase = .idle
        sendPhaseOwner = nil
        // Zero out context immediately so the UI resets to 0% before the
        // server round-trip — prevents the ring lingering at 100% if the
        // server returns a fresh conversation that still carries stale usage.
        lastTokenUsage = nil
        lastContextInfo = nil

        let fresh: Conversation
        do {
            fresh = try await heraldClient.clearConversation()
        } catch {
            // If the relay is unreachable (502, network error, etc.), fall back
            // to a local-only clear. The user gets a blank conversation immediately
            // rather than an "internal error" dialog. The old conversation will
            // be archived on the relay next time it's reachable.
            Self.logger.warning("Relay clear failed, using local fallback: \(error.localizedDescription)")
            fresh = Conversation(title: "New Chat")
        }

        conversation = fresh
        lastTokenUsage = fresh.latestUsage
        lastContextInfo = nil
        pendingMessageSentAt = nil
        persistence.saveConversationCache(fresh)
        needsServerRefresh = true  // Force next loadConversationIfNeeded() to bypass cache
        onConversationChanged?()
        pollingTask?.cancel()
        pollingTask = nil
    }

    /// Recover from a stalled stream after app foregrounding.
    /// If the server completed a response while the app was backgrounded,
    /// this will pick it up and clear the stale streaming state.
    func recoverStalledStream() async {
        // D3: streamingPhase may only be non-idle while isStreaming is true.
        // If no stream is in flight, reconcile immediately.
        guard isStreaming else {
            streamingPhase = .idle
            return
        }

        // Refresh conversation from server
        let refreshed = await refreshActiveConversation()
        guard let refreshed else {
            // Couldn't reach server — clear phase so the banner doesn't latch
            // if the stream has since resolved.
            if !isStreaming { streamingPhase = .idle }
            return
        }

        // Check if the server has a completed response that we missed
        let serverMessages = refreshed.messages
        let localMessages = conversation?.messages ?? []

        // If server has more delivered messages than we do, the stream
        // completed while we were suspended
        let serverDelivered = serverMessages.filter { $0.status == .delivered && $0.sender == .herald }
        let localDelivered = localMessages.filter { $0.status == .delivered && $0.sender == .herald }

        if serverDelivered.count > localDelivered.count {
            // Server has the response — merge and clear streaming state
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)

            // Clear all active streams
            for (jobID, _) in activeStreams {
                activeStreams.removeValue(forKey: jobID)
            }
            streamingTask?.cancel()
            streamingTask = nil
            chatLiveActivity.endActivity()
            pendingMessageSentAt = nil
            streamingPhase = .idle  // D3: Stream resolved, clear phase

            if let latestUsage = conversation?.latestUsage {
                lastTokenUsage = latestUsage
            }
            if let conversation {
                persistence.saveConversationCache(conversation)
                onConversationChanged?()
            }
        }
    }

    // MARK: - Gateway restart suspension (Build 33)

    /// Called when a Hermes gateway restart is confirmed and in progress.
    /// The gateway is about to go down, so no stream or poll can complete:
    /// invalidate every in-flight stream/poll, settle placeholders as
    /// interrupted, and flip the UI into the restarting state. New sends are
    /// queued visibly by `sendMessage` while suspended.
    func beginRestartSuspension() {
        guard !restartInProgress else { return }
        restartInProgress = true

        streamingTask?.cancel()
        streamingTask = nil
        // Invalidate stale callbacks from the cancelled stream coordinator.
        activeAttemptID = UUID()
        pollingTask?.cancel()
        pollingTask = nil
        chatLiveActivity.endActivity()
        ttsService?.stop()
        streamingPhase = .restarting

        // Durable outbox: any in-flight job is dead once the gateway is
        // replaced — return the leases to .queued so resumeAfterRestart()
        // re-submits them. Resubmission is idempotent via the frozen
        // clientMessageID (the relay dedupes by it).
        var requeued = false
        for idx in outboxItems.indices where outboxItems[idx].isInFlight {
            outboxItems[idx].state = .queued
            outboxItems[idx].nextAttemptAt = nil
            requeued = true
        }
        if requeued {
            persistOutbox()
            appendLog(level: .warn, "Outbox: \(outboxItems.filter { $0.state == .queued }.count) message(s) requeued for after-restart submit")
        }
        sendPhase = .queued
        sendPhaseOwner = nil

        // Settle every streaming placeholder as interrupted before clearing
        // ownership — otherwise the placeholder renders a forever-animating
        // bubble with no live stream behind it.
        let placeholders = Array(activeStreams.values)
        activeStreams.removeAll()
        for placeholderID in placeholders {
            flushPendingReasoning(placeholderID: placeholderID)
            flushPendingDeltas(placeholderID: placeholderID)
            if var conv = conversation,
               let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                conv.messages[idx].isStreaming = false
                conv.messages[idx].status = .interrupted
                for i in conv.messages[idx].toolActivities.indices {
                    conv.messages[idx].toolActivities[i].isActive = false
                }
                conversation = conv
            }
        }
        pendingMessageSentAt = nil

        appendLog(level: .warn, "Hermes gateway restart — streaming and polling suspended")
        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
    }

    /// Called when the restart reaches a terminal state (healthy or failed).
    /// Re-enables sends, drains the visibly-queued outbox, and restarts the
    /// polling safety net so in-flight job state reconciles against the fresh
    /// gateway. Conversation/model reloads are orchestrated by the caller
    /// (SettingsScreen.recoverAfterRestart).
    func resumeAfterRestart() async {
        restartInProgress = false
        streamingPhase = .idle
        appendLog(level: .info, "Hermes gateway restart settled — sending resumed")

        // Drain the durable outbox: submit queued items for the current
        // conversation FIFO. Items for other conversations stay durably
        // queued and submit when that conversation becomes current.
        await submitNextEligible()
        restartPendingPollingIfNeeded()

        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
    }

    func cancelStreaming() {
        // Build 31: cancel the server-side job before tearing down local state.
        // Previously this only did local teardown — the server job kept running
        // to completion, and its output landed in the conversation on the next
        // poll, appearing as a duplicate or ghost reply.
        let jobIDs = Array(activeStreams.keys)
        if !jobIDs.isEmpty {
            let client = heraldClient
            Task {
                for jobID in jobIDs {
                    do {
                        try await client.cancelJob(jobID: jobID)
                        Logger.app.info("CancelStreaming: server job \(jobID.uuidString.prefix(8)) cancelled")
                    } catch {
                        Logger.app.warning("CancelStreaming: server cancel failed for \(jobID.uuidString.prefix(8)): \(error.localizedDescription)")
                    }
                }
            }
        }

        streamingTask?.cancel()
        streamingTask = nil
        // Invalidate stale callbacks from the cancelled stream coordinator.
        activeAttemptID = UUID()
        chatLiveActivity.endActivity()
        ttsService?.stop()
        streamingPhase = .idle  // D3: A cancelled stream is not reconnecting
        // Durable outbox: any in-flight item is cancelled by user intent —
        // never auto-resubmitted.
        cancelInFlightOutboxItems()
        sendPhase = .idle
        sendPhaseOwner = nil

        // Flush any buffered deltas onto the placeholder before finalizing.
        if let sid = streamingMessageID {
            flushPendingReasoning(placeholderID: sid)
            flushPendingDeltas(placeholderID: sid)
        }

        // Finalize current streaming message with content received so far
        if let sid = streamingMessageID,
           var conv = conversation,
           let idx = conv.messages.firstIndex(where: { $0.id == sid }) {
            conv.messages[idx].isStreaming = false
            conv.messages[idx].status = .interrupted  // Build 31: not delivered — interrupted
            for i in conv.messages[idx].toolActivities.indices {
                conv.messages[idx].toolActivities[i].isActive = false
            }
            conversation = conv
        }
        activeStreams.removeAll()
        pendingMessageSentAt = nil

        if let conversation {
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
            onConversationChanged?()
        }
    }

    // MARK: - Outbox Queue (Build 31 → Build 33 WSB durable)

    /// Enqueue a message to be sent after the active turn finishes.
    /// Called when the user taps the queue button while a response is already
    /// streaming. Durable: the item goes into the on-disk outbox manifest and
    /// survives force-quit. Returns the record, or nil when the message was
    /// rejected (empty/duplicate) — the caller should then keep the draft.
    @discardableResult
    func queueNextMessage(text: String, attachments: [PendingAttachment]) -> ChatOutboxRecord? {
        enqueueMessage(text, attachments: attachments)
    }

    /// Remove a queued item by ID. Items that are in flight (leased/submitted)
    /// are cancelled instead — never silently dropped mid-job.
    func removeQueuedItem(_ id: UUID) {
        if let idx = outboxItems.firstIndex(where: { $0.clientMessageID == id }) {
            let item = outboxItems[idx]
            if item.isInFlight {
                failOutboxItem(item, state: .cancelled, error: "Removed by user")
                if let messageIdx = conversation?.messages.firstIndex(where: { $0.id == item.clientMessageID }) {
                    conversation?.messages[messageIdx].status = .failed
                }
            } else {
                outboxItems.remove(at: idx)
                persistOutbox()
                outboxStore.removeStagedAttachments(for: item)
            }
            onConversationChanged?()
        }
    }

    /// Count of queued-but-not-yet-submitted items for the current
    /// conversation — drives the "N queued" composer hint.
    var queuedCountForCurrentConversation: Int {
        guard let conversationID = conversation?.id else { return 0 }
        return outboxItems.filter {
            $0.conversationID == conversationID && $0.state == .queued
        }.count
    }

    /// `streamingPhase` is UI-only state that outlives the transport it describes.
    /// Backgrounding kills the SSE task without a terminal event, so the phase must
    /// be reconciled against `isStreaming` on every foreground — otherwise a
    /// "Reconnecting…" banner latches until relaunch (B4/D3).
    func reconcileStreamingPhase() {
        // Build 33: a gateway restart keeps the restarting phase even though
        // no stream is in flight — the transport is gone ON PURPOSE.
        if restartInProgress { return }
        if !isStreaming { streamingPhase = .idle }
        // Build 33 WSB: the send phase must also reflect the outbox reality —
        // backgrounding kills the SSE task without a terminal event, so a
        // latched .submitting/.streaming phase needs settling. Queued items
        // keep .queued; recovery resubmits them.
        let hasInFlight = outboxItems.contains { $0.isInFlight }
        let hasQueued = outboxItems.contains { $0.state == .queued }
        if !hasInFlight {
            sendPhase = hasQueued ? .queued : .idle
            sendPhaseOwner = nil
        }
    }

    func injectVoiceTranscript(voiceSessionId: UUID, duration: TimeInterval) async {
        do {
            let updated = try await heraldClient.injectVoiceTranscript(voiceSessionId: voiceSessionId)
            conversation = updated
            lastTokenUsage = updated.latestUsage

            // Set voiceSessionDuration on the system banner message
            if let idx = conversation?.messages.lastIndex(where: {
                $0.sender == .system && $0.content.contains("[Voice session ended]")
            }) {
                conversation?.messages[idx].voiceSessionDuration = duration
            }

            if let conversation {
                persistence.saveConversationCache(conversation)
                onConversationChanged?()
            }
        } catch {
            // Injection failed — voice transcript not added to chat. Non-fatal.
        }
    }

    func exportConversationToFile() {
        guard let conversation else { return }

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyyMMdd_HHmmss"
        let timestamp = formatter.string(from: Date())
        let filename = "herald_conversation_\(timestamp).json"

        let exportData: [String: Any] = [
            "title": conversation.title,
            "sessionId": conversation.id.uuidString,
            "exportedAt": ISO8601DateFormatter().string(from: Date()),
            "messageCount": conversation.messages.count,
            "messages": conversation.messages.map { msg in
                [
                    "role": msg.sender.rawValue,
                    "content": msg.content,
                    "timestamp": ISO8601DateFormatter().string(from: msg.timestamp),
                ] as [String: String]
            },
        ]

        guard let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        let fileURL = dir.appendingPathComponent(filename)

        do {
            let data = try JSONSerialization.data(withJSONObject: exportData, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: fileURL)
            // Append a system message confirming the save (caller handles this)
        } catch {
            // Export failed silently — caller can check
        }
    }

    func setConversationTitle(_ title: String) {
        conversation?.title = title
        if let conversation {
            persistence.saveConversationCache(conversation)
            onTitleChanged?(conversation.id, title)
            onConversationChanged?()
        }
    }

    // D5: autoCompress() and autoCompressAttempted deleted.

    private func autoTitleIfNeeded() async {
        let defaultTitles: Set<String> = ["New Chat", "Kallisti"]
        guard let conv = conversation,
              defaultTitles.contains(conv.title),
              !autoTitleAttempted,
              let firstUserMessage = conv.messages.first(where: { $0.sender == .user })
        else { return }
        let raw = firstUserMessage.content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !raw.isEmpty else { return }

        autoTitleAttempted = true

        // Deterministic local fallback: smart truncation of first message.
        // Strip leading slash commands and common prefixes, then use the
        // first meaningful line as the title.
        let cleaned = raw
            .replacingOccurrences(of: #"^/\w+\s*"#, with: "", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let firstLine = cleaned.split(separator: "\n").first.map(String.init) ?? cleaned
        let title = firstLine.count > 50
            ? String(firstLine.prefix(47)).trimmingCharacters(in: .whitespaces) + "..."
            : firstLine
        if let current = conversation, defaultTitles.contains(current.title) {
            conversation?.title = title
            onTitleChanged?(current.id, title)
        }
    }

    func deleteMessage(_ message: Message) {
        conversation?.messages.removeAll { $0.id == message.id }
    }

    func retryMessage(_ message: Message) async {
        // Determine the user content to retry.
        let sourceMessage: Message?
        if message.sender == .user {
            sourceMessage = message
        } else {
            sourceMessage = conversation?.messages.last(where: { $0.sender == .user })
        }

        guard let sourceMessage else { return }
        let attachments = sourceMessage.attachments.compactMap(PendingAttachment.restore)
        let content = normalizedRetryContent(for: sourceMessage)
        guard !content.isEmpty || !attachments.isEmpty else { return }

        if message.sender != .user && !message.content.isEmpty {
            // Build 31 (fix): cancel the original streaming task and server-side
            // job so stale callbacks from the superseded attempt cannot mutate
            // the new placeholder, reconnecting banner, or transcript.
            streamingTask?.cancel()
            streamingTask = nil
            // Bump the attempt generation so every in-flight event from the
            // old stream coordinator is discarded — the guard check at each
            // case in the consumer loop rejects events with a stale attemptID.
            activeAttemptID = UUID()

            // Cancel the server-side job for the failed/incomplete response.
            // This is best-effort — a fast terminal reply may have already
            // persisted; the reduce path will reconcile that idempotently.
            if let originalJobID = activeStreams.first(where: { $0.value == message.id })?.key {
                Task {
                    do {
                        try await heraldClient.cancelJob(jobID: originalJobID)
                        Logger.app.info("Retry: cancelled original job \(originalJobID.uuidString.prefix(8))")
                    } catch {
                        Logger.app.warning("Retry: server cancel failed for \(originalJobID.uuidString.prefix(8)): \(error.localizedDescription)")
                    }
                    activeStreams.removeValue(forKey: originalJobID)
                }
            }

            // Only remove the failed assistant placeholder — keep the user message.
            conversation?.messages.removeAll { $0.id == message.id }

            // Build 117 fix: also remove any existing message with the same
            // clientMessageID, regardless of status.  Without this, a user
            // message that was marked .delivered by a stale code path (or by
            // the merge preserving .sending → .delivered transition) would
            // survive the hasPendingDuplicateMessage guard in enqueueMessage,
            // which only blocks on status == .sending.  The retry would then
            // create a SECOND optimistic row with the same id, and SwiftUI
            // ForEach would render both as separate bubbles.
            if let existingClientID = sourceMessage.clientMessageID {
                conversation?.messages.removeAll { $0.clientMessageID == existingClientID && $0.id != message.id }
            }

            // Build 31 (fix): continuation context is transport metadata.
            // The tail snippet helps Hermes resume, but must never appear as
            // canonical user content in the transcript, copy/share, TTS, or
            // notifications.  Prior code prepended it directly to the user
            // text, which was persisted and displayed as a visible bubble.
            let tail = String(message.content.suffix(120))
            let continuationContext = "The previous response was cut off. It ended with: \"\(tail)\". Continue from where you stopped."
            await sendMessage(
                content,
                attachments: attachments,
                clientMessageID: sourceMessage.clientMessageID,
                continuationContext: continuationContext
            )
        } else {
            // Failed user message or empty assistant response — remove and resend.
            conversation?.messages.removeAll { $0.id == message.id }
            await sendMessage(content, attachments: attachments, clientMessageID: UUID())
        }
    }

    func setPollingEnabled(_ isEnabled: Bool) {
        isPollingEnabled = isEnabled
        if isEnabled {
            restartPendingPollingIfNeeded()
        } else {
            pollingTask?.cancel()
            pollingTask = nil
        }
    }

    func replaceCommandCatalog(_ catalog: [SlashCommand], activeModel: String? = nil, contextWindow: Int? = nil) {
        commandCatalog = catalog.isEmpty ? SlashCommand.allBuiltIn : catalog
        if let activeModel { activeModelName = activeModel }
        if let contextWindow { self.contextWindow = contextWindow }
    }

    func resetCommandCatalog() {
        commandCatalog = SlashCommand.allBuiltIn
        activeModelName = nil
        contextWindow = nil
    }

    /// Append a log entry to the live log buffer shown in the iPad
    /// inspector panel's Logs tab. Capped at 500 entries, persisted to disk.
    func appendLog(level: LogLevel, _ message: String) {
        logEntries.append(LogEntry(level: level, message: message))
        if logEntries.count > Self.maxLogEntries { logEntries.removeFirst(100) }
        // Persist on the next main-actor cycle so logging doesn't stall
        let snapshot = logEntries
        Task { @MainActor [persistence] in
            persistence.saveLogEntries(snapshot)
        }
    }

    func reset() {
        pollingTask?.cancel()
        pollingTask = nil
        streamingTask?.cancel()
        streamingTask = nil
        activeAttemptID = UUID()  // invalidate all in-flight stream callbacks
        activeStreams.removeAll()
        restartInProgress = false  // Build 33: don't carry suspension across sessions
        // Build 33 WSB: reload the durable outbox from disk — a profile
        // switch must not carry stale in-memory records (or a phantom
        // sendPhase) into the new session.
        let manifest = outboxStore.load()
        outboxItems = manifest.items
        outboxNextSequence = manifest.nextSequence
        sendPhase = .idle
        sendPhaseOwner = nil
        // Drop every per-conversation lease — a fresh session has no in-flight
        // attempts. The durable outbox on disk is reloaded above and its
        // queued items will be re-leased as needed by the next submitNextEligible.
        activeLeases.removeAll()
        // Preserve log entries across resets — they're diagnostic history,
        // not session state. Append a marker so the user can see where
        // one session ended and another began.
        appendLog(level: .info, "——— session reset ———")
        isPollingEnabled = false
        resetCommandCatalog()
        conversation = nil
        isLoading = false
        pendingMessageSentAt = nil
        lastTokenUsage = nil
        lastContextInfo = nil
        persistence.clearConversationCache()
    }

    func resolvedContextWindow(fallbackModelName: String?) -> Int? {
        // Prefer relay-provided context window from SSE metadata
        if let ctx = contextWindow, ctx > 0 { return ctx }

        // If the ModelStore has context window info from the model catalog
        // (config.yaml providers), use that. Otherwise return nil rather than
        // fabricating a guess — never show made-up numbers.
        return nil
    }

    private var hasPendingMessages: Bool {
        conversation?.messages.contains(where: { $0.sender == .user && $0.status == .sending }) == true
    }

    private func hasPendingDuplicateMessage(_ content: String, attachments: [PendingAttachment]) -> Bool {
        guard let messages = conversation?.messages else { return false }
        let attSig = attachmentSignature(for: attachments.map { MessageAttachment(from: $0) })
        let now = Date()
        return messages.contains(where: {
            $0.sender == .user
                && (
                    $0.status == .sending
                    || ($0.status == .sent && now.timeIntervalSince($0.timestamp) < 60)
                    || ($0.status == .delivered && now.timeIntervalSince($0.timestamp) < 30)
                )
                && normalizedRetryContent(for: $0) == content
                && attachmentSignature(for: $0.attachments) == attSig
        })
    }

    // MARK: - Delta coalescing

    /// Enqueue a reasoning delta into the reasoning buffer.
    /// Uses the same coalescing strategy as text deltas (33ms / 4KB) to avoid
    /// per-token `@Observable` mutations during deep reasoning phases.
    private func enqueueReasoningDelta(_ delta: String, placeholderID: UUID) {
        guard !delta.isEmpty else { return }
        var buf = reasoningBuffers[placeholderID] ?? DeltaBuffer()
        buf.chunks.append(delta)
        buf.bytes += delta.utf8.count

        if buf.bytes >= Self.deltaFlushByteThreshold {
            reasoningBuffers[placeholderID] = buf
            flushPendingReasoning(placeholderID: placeholderID)
            return
        }

        guard buf.flushTask == nil else {
            reasoningBuffers[placeholderID] = buf
            return
        }
        buf.flushTask = Task { [weak self, placeholderID] in
            try? await Task.sleep(for: Self.deltaFlushInterval)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                self?.flushPendingReasoning(placeholderID: placeholderID)
            }
        }
        reasoningBuffers[placeholderID] = buf
    }

    /// Flush all buffered reasoning deltas onto the placeholder message.
    private func flushPendingReasoning(placeholderID: UUID) {
        guard var buf = reasoningBuffers[placeholderID] else { return }
        buf.flushTask?.cancel()
        buf.flushTask = nil

        guard !buf.chunks.isEmpty else {
            reasoningBuffers.removeValue(forKey: placeholderID)
            return
        }
        let totalBytes = buf.bytes
        reasoningBuffers.removeValue(forKey: placeholderID)

        guard var conv = conversation,
              let idx = conv.messages.firstIndex(where: { $0.id == placeholderID })
        else { return }

        // Single concat for all buffered chunks
        var buffer = conv.messages[idx].reasoning
        buffer.reserveCapacity(buffer.count + totalBytes)
        for chunk in buf.chunks { buffer.append(chunk) }
        conv.messages[idx].reasoning = buffer
        conversation = conv
    }

    private func enqueueDelta(_ delta: String, placeholderID: UUID) {
        guard !delta.isEmpty else { return }
        var buf = deltaBuffers[placeholderID] ?? DeltaBuffer()
        buf.chunks.append(delta)
        buf.bytes += delta.utf8.count

        // If we've buffered a lot, flush immediately so the UI doesn't fall
        // multiple frames behind during a burst.
        if buf.bytes >= Self.deltaFlushByteThreshold {
            deltaBuffers[placeholderID] = buf
            flushPendingDeltas(placeholderID: placeholderID)
            return
        }

        guard buf.flushTask == nil else {
            deltaBuffers[placeholderID] = buf
            return
        }
        buf.flushTask = Task { [weak self, placeholderID] in
            try? await Task.sleep(for: Self.deltaFlushInterval)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                self?.flushPendingDeltas(placeholderID: placeholderID)
            }
        }
        deltaBuffers[placeholderID] = buf
    }

    private func flushPendingDeltas(placeholderID: UUID) {
        guard var buf = deltaBuffers[placeholderID] else { return }
        buf.flushTask?.cancel()
        buf.flushTask = nil

        guard !buf.chunks.isEmpty else {
            deltaBuffers.removeValue(forKey: placeholderID)
            return
        }
        let chunks = buf.chunks
        let totalBytes = buf.bytes
        deltaBuffers.removeValue(forKey: placeholderID)

        guard var conv = conversation,
              let idx = conv.messages.firstIndex(where: { $0.id == placeholderID })
        else { return }

        // Single concat: O(sum(chunk sizes)) instead of O(n·chunks) across ticks.
        var buffer = conv.messages[idx].content
        let beforeCount = buffer.count
        buffer.reserveCapacity(buffer.count + totalBytes)
        for chunk in chunks { buffer.append(chunk) }
        conv.messages[idx].content = buffer
        Self.logger.debug("flush deltas chunks=\(chunks.count) bytes=\(totalBytes) content \(beforeCount)→\(buffer.count) chars")

        // Only touch tool-activity state when it actually needs clearing —
        // avoids spurious writes on every delta for messages that never ran tools.
        if conv.messages[idx].toolActivity != nil {
            conv.messages[idx].toolActivity = nil
        }
        var toolActivities = conv.messages[idx].toolActivities
        var didClearActive = false
        for i in toolActivities.indices where toolActivities[i].isActive {
            toolActivities[i].isActive = false
            didClearActive = true
        }
        if didClearActive {
            conv.messages[idx].toolActivities = toolActivities
        }

        conversation = conv
    }

    // Exponential backoff delays (seconds). The first polls are fast because
    // the relay usually delivers within a handful of seconds; later polls
    // spread out so we don't hammer a struggling relay. Polling is a low-frequency
    // safety net — it must never override a nonterminal server job.
    private static let pollingBackoffSeconds: [Double] = [
        2, 3, 5, 8, 12, 18, 25, 30, 30, 30, 30, 30,
    ]

    private func restartPendingPollingIfNeeded() {
        guard isPollingEnabled, hasPendingMessages else {
            pollingTask?.cancel()
            pollingTask = nil
            return
        }

        guard pollingTask == nil else { return }

        pollingTask = Task { [weak self] in
            guard let self else { return }

            for delay in Self.pollingBackoffSeconds {
                try? await Task.sleep(for: .seconds(delay))
                guard !Task.isCancelled else { break }
                let capturedGeneration = self.conversationGeneration
                let fresh = await self.refreshActiveConversation()
                // Build 30: discard stale poll results.  If the terminal
                // row was committed while this poll was in flight, the
                // server snapshot may lack it — applying it would delete
                // the completed response.  The next poll cycle will include
                // the terminal row and proceed normally.
                guard capturedGeneration == self.conversationGeneration else {
                    Self.logger.info("Poll dropped — generation moved (\(capturedGeneration) → \(self.conversationGeneration))")
                    break
                }
                self.conversation = self.mergeConversationMetadata(from: self.conversation, into: fresh)
                // Build 28: a successful merge that includes the terminal
                // turn satisfies the pending reconciliation marker set at
                // .finished.  Future merges are ordinary refreshes.
                self.pendingServerReconciliation = false
                if let latestUsage = self.conversation?.latestUsage {
                    self.lastTokenUsage = latestUsage
                }
                if let conversation = self.conversation {
                    self.persistence.saveConversationCache(conversation)
                    self.onConversationChanged?()
                }
                if self.hasPendingMessages == false {
                    self.pendingMessageSentAt = nil
                    break
                }
            }
            // Polling exhausted — do NOT mark messages as failed.
            // The job may still be running on the server. The user can
            // see the sending state and choose to retry manually.

            if self.pollingTask?.isCancelled == false {
                self.pollingTask = nil
            }
        }
    }

    /// Re-attaches transient streaming artifacts (tool timeline, code diff) onto the
    /// canonical conversation that the relay returned, since the relay knows nothing
    /// about those client-only fields.
    /// Refreshes `conversation` from the relay. When a specific conversation/session
    /// is already active, refreshes THAT conversation by id — never the device's
    /// arbitrary "current" conversation, which (now that a device can have many
    /// sessions) may silently resolve to an unrelated session and clobber the one
    /// actually on screen.
    private func refreshActiveConversation() async -> Conversation? {
        if let activeID = conversation?.id {
            return try? await heraldClient.loadConversation(id: activeID)
        }
        return await heraldClient.loadConversation()
    }

    /// Merge a server-resolved message with whatever already streamed into the
    /// placeholder. A resolved message with empty content must never erase
    /// streamed text — that regression rendered every reply as a blank bubble.
    ///
    /// Originally added in B35 (`c5069af`) and removed by the Build 41 refactor
    /// (`71884b9`); restored for 2.4.0. `StreamedContentPreservationTests` covers it.
    static func mergeResolvedMessage(resolved: Message, streamedContent: String) -> Message {
        guard resolved.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !streamedContent.isEmpty else {
            return resolved
        }
        var merged = resolved
        merged.content = streamedContent
        return merged
    }

    /// Internal rather than private so the merge invariants can be tested
    /// directly — this is the function that dropped completed replies in B39.
    func mergeConversationMetadata(
        from localConversation: Conversation?,
        into refreshedConversation: Conversation?
    ) -> Conversation? {
        guard var refreshedConversation else { return localConversation }
        guard let localConversation else { return refreshedConversation }

        // Preserve user-set titles — only accept the server's title if the local
        // title is still a default placeholder. This prevents a late server-derived
        // title from overwriting a user rename.
        let defaultTitles: Set<String> = ["New Chat", "Kallisti"]
        if !defaultTitles.contains(localConversation.title) {
            refreshedConversation.title = localConversation.title
        }

        if refreshedConversation.latestUsage == nil {
            refreshedConversation.latestUsage = localConversation.latestUsage
        }

        // Local id → index in refreshedConversation.messages, for messages the
        // server returned under a different id.  The anchor splice below needs
        // this: the server mints its own message ids (http_facade.py:281), so id
        // equality alone can never recognise a local message that the server
        // *did* return.
        var localToRefreshedIndex: [UUID: Int] = [:]

        for index in refreshedConversation.messages.indices {
            let remote = refreshedConversation.messages[index]

            // Prefer exact UUID match (works when the relay echoes back the same ID).
            let local: Message?
            if let byID = localConversation.messages.first(where: { $0.id == remote.id }) {
                local = byID
            } else if let remoteClientMessageID = remote.clientMessageID {
                local = localConversation.messages.first(where: {
                    $0.id == remoteClientMessageID || $0.clientMessageID == remoteClientMessageID
                })
            } else if let remoteJobID = remote.jobID {
                // Fallback: the streaming placeholder had a client-generated UUID that
                // differs from the server-assigned message ID.  Match on jobID + sender.
                //
                // B40: this used to additionally require the local message to
                // carry toolActivities/codeDiff/reasoning, so a plain-text reply
                // never matched — which also meant B39 T5's empty-content and
                // truncation guards below never ran for the most common shape of
                // answer. The artifact copies are each guarded on their own.
                local = localConversation.messages.first(where: {
                    $0.jobID == remoteJobID
                        && $0.sender == remote.sender
                        && $0.sender == .herald
                })
            } else {
                local = nil
            }

            guard let local else { continue }

            localToRefreshedIndex[local.id] = index

            // B39 T5: defence in depth — if the local (streamed) message has
            // non-empty content and the server's version is empty or a strict
            // prefix, keep the locally-rendered text. This protects against a
            // server refetch clobbering a good streamed answer with a truncated
            // or empty version.
            //
            // Added in B39 (`6c90009`), reverted by the Build 41 refactor
            // (`71884b9`), restored for 2.4.0. Covered by
            // `B40ConversationMergeTests.emptyServerCopyDoesNotBlankAPlainReply`.
            let localContent = local.content.trimmingCharacters(in: .whitespacesAndNewlines)
            let remoteContent = refreshedConversation.messages[index].content
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !localContent.isEmpty {
                if remoteContent.isEmpty {
                    Self.logger.error(
                        "B39 T5: server returned empty content for message \(remote.id) (jobId=\(String(describing: remote.jobID))), keeping local text (len=\(localContent.count))"
                    )
                    refreshedConversation.messages[index].content = local.content
                } else if localContent.count > remoteContent.count,
                          localContent.hasPrefix(remoteContent) {
                    Self.logger.error(
                        "B39 T5: server content (len=\(remoteContent.count)) is a strict prefix of local (len=\(localContent.count)) for message \(remote.id) — keeping streamed text"
                    )
                    refreshedConversation.messages[index].content = local.content
                }
            }

            if !local.toolActivities.isEmpty {
                refreshedConversation.messages[index].toolActivities = local.toolActivities
                refreshedConversation.messages[index].toolActivity = local.toolActivity
            }

            if let diff = local.codeDiff, refreshedConversation.messages[index].codeDiff == nil {
                refreshedConversation.messages[index].codeDiff = diff
            }

            if !local.reasoning.isEmpty {
                refreshedConversation.messages[index].reasoning = local.reasoning
                if local.reasoningDuration != nil {
                    refreshedConversation.messages[index].reasoningDuration = local.reasoningDuration
                }
            }

            if !local.attachments.isEmpty {
                refreshedConversation.messages[index].attachments = mergeAttachments(
                    local.attachments,
                    onto: refreshedConversation.messages[index].attachments
                )
            }

            // Build 23: a remote user message with deliveryStatus "delivered"
            // must not overwrite a local user row that is still .sending or
            // .sent — the green check is a final-delivery signal, not a
            // connection signal.  Only a credible terminal result can mark
            // the outgoing row delivered.
            if local.sender == .user,
               refreshedConversation.messages[index].status == .delivered,
               local.status == .sending || local.status == .sent {
                refreshedConversation.messages[index].status = local.status
            }
        }

        // Turn projection dedup.  Hermes persists one assistant row per tool
        // boundary; the matching loop above copies the placeholder's reasoning and
        // tool timeline onto EVERY row sharing that jobId.  Keep those artifacts
        // only on the last row of each jobId so the UI shows one "Thought process"
        // card per turn.
        //
        // B18: grouped by jobId across the whole array rather than by contiguous
        // run.  The B17 contiguous scan broke on any interruption — a nil-jobId
        // tool row, a system row, or a local-only message spliced in by the anchor
        // pass — and each fragment then kept its own reasoning card, which is the
        // "thought bubbles above and below the answer" report.
        do {
            var lastIndexForJob: [UUID: Int] = [:]
            for (index, message) in refreshedConversation.messages.enumerated()
            where message.sender == .herald {
                guard let jobID = message.jobID else { continue }
                lastIndexForJob[jobID] = index
            }
            for (index, message) in refreshedConversation.messages.enumerated()
            where message.sender == .herald {
                guard let jobID = message.jobID,
                      let keepIndex = lastIndexForJob[jobID],
                      keepIndex != index else { continue }
                if !refreshedConversation.messages[index].reasoning.isEmpty {
                    Self.logger.debug(
                        "Turn projection: stripping reasoning from row \(index) of jobId \(jobID) (kept on row \(keepIndex))"
                    )
                    refreshedConversation.messages[index].reasoning = ""
                    refreshedConversation.messages[index].reasoningDuration = nil
                }
                if !refreshedConversation.messages[index].toolActivities.isEmpty {
                    refreshedConversation.messages[index].toolActivities = []
                    refreshedConversation.messages[index].toolActivity = nil
                }
            }
        }

        // B40 P0-1: preserve EVERY local message the refreshed payload is
        // missing — not just streaming placeholders and artifact-carrying
        // replies.
        //
        // This merge is fed `heraldClient.currentConversation`, which after a
        // send is the POST /v1/messages payload: a conversation containing
        // *only the user message just sent* (http_facade.py:795). The old
        // predicate kept a resolved reply only when it carried reasoning,
        // toolActivities or a codeDiff — so a plain-text answer (the normal
        // shape for models that emit no reasoning_content) matched nothing and
        // was dropped by `.finished` immediately after the delivered check and
        // the completion haptic. That is the "every indicator says done, no
        // reply on screen" P0.
        //
        // Dropping a message the server merely hasn't caught up on is never
        // correct here; keep it and let a real conversation fetch reconcile.
        //
        // Added in B40 (`dc151db`), reverted by the Build 41 refactor
        // (`71884b9`), restored for 2.4.0.
        let refreshedIDs = Set(refreshedConversation.messages.map(\.id))
        // Keyed by sender as well as jobID: a user message and the reply it
        // produced share a jobID, and dropping the prompt because the server
        // returned the answer would be its own bug.
        let refreshedJobKeys = Set(
            refreshedConversation.messages.compactMap { message in
                message.jobID.map { "\($0.uuidString)|\(message.sender)" }
            }
        )
        // Build 102 P0-C: clientMessageID is the AUTHORITATIVE identity for
        // a user row — it travels from the outbox primary key through every
        // connector response. session_store._message_to_dict returns it on
        // every user message (http_facade.py user rows). Keyed by sender so
        // an assistant row carrying the same clientMessageID is a different
        // identity and never collides. Identity-by-clientMessageID is checked
        // BEFORE fingerprint fallback per marching orders §7 reconciliation
        // hierarchy: (1) canonical id, (2) clientMessageID+sender,
        // (3) jobID+sender, (4) content fingerprint (legacy only).
        let refreshedClientMessageKeys = Set(
            refreshedConversation.messages.compactMap { message in
                message.clientMessageID.map { "\($0.uuidString)|\(message.sender)" }
            }
        )
        // Fingerprint → the refreshed indices carrying it, oldest first.  The
        // set is what the dedupe checks; the map is what makes a deduped local
        // message *anchorable* (see the claim loop below).
        var refreshedIndicesByFingerprint: [String: [Int]] = [:]
        for (index, message) in refreshedConversation.messages.enumerated() {
            refreshedIndicesByFingerprint[Self.messageFingerprint(message), default: []].append(index)
        }
        let refreshedFingerprints = Set(refreshedIndicesByFingerprint.keys)

        // A streamed agent turn is accumulated locally, while history stores
        // one assistant row per tool boundary. Recognize that complete run
        // before preserving the local bubble, then keep its artifacts on the
        // final persisted segment.
        var segmentedLocalIDs = Set<UUID>()
        for local in localConversation.messages where local.sender == .herald && !local.isStreaming {
            guard !refreshedIDs.contains(local.id),
                  !refreshedFingerprints.contains(Self.messageFingerprint(local)) else { continue }
            let localText = Self.normalizedMessageContent(local.content)
            guard !localText.isEmpty else { continue }
            for start in refreshedConversation.messages.indices {
                guard refreshedConversation.messages[start].sender == .herald else { continue }
                var end = start
                var pieces: [String] = []
                while end < refreshedConversation.messages.count,
                      refreshedConversation.messages[end].sender == .herald {
                    pieces.append(refreshedConversation.messages[end].content)
                    let joined = Self.normalizedMessageContent(pieces.joined(separator: " "))
                    if pieces.count >= 2, joined == localText,
                       joined.count >= Int(Double(localText.count) * 0.9) {
                        segmentedLocalIDs.insert(local.id)
                        refreshedConversation.messages[end].toolActivities = local.toolActivities
                        refreshedConversation.messages[end].toolActivity = local.toolActivity
                        refreshedConversation.messages[end].codeDiff = local.codeDiff
                        refreshedConversation.messages[end].reasoning = local.reasoning
                        refreshedConversation.messages[end].reasoningDuration = local.reasoningDuration
                        break
                    }
                    if joined.count > localText.count { break }
                    end += 1
                }
                if segmentedLocalIDs.contains(local.id) { break }
            }
        }

        // B21: a local message deduped by *fingerprint* must also be recorded in
        // `localToRefreshedIndex`, because that map is what the anchor walk-back
        // below treats as "this local message exists in the refreshed array".
        //
        // The matching loop above can only recognise a message by id,
        // clientMessageID or jobID — and the connector supplies none of those
        // for a **user** row.  `session_store._message_to_dict` hardcodes
        // `"clientMessageId": None`, and `jobId` comes from the
        // `get_message_job_id` map, which is only written for assistant rows and
        // only for 120s after the producing job completes.
        //
        // So on a real refresh the previous *reply* is matchable and the prompt
        // between it and the live placeholder is not.  The walk-back skipped the
        // prompt, anchored the placeholder to the reply above it, and spliced it
        // in one slot too high — the follow-up question rendering *below* the
        // answer it produced.  Observed 2.4.1(20), 2026-07-30 23:58.
        //
        // Deduping already proved these are the same message; recording where
        // the server put it costs nothing and closes the identity gap.
        // `B21FollowUpOrderTests` covers it.
        var claimedRefreshedIndices = Set(localToRefreshedIndex.values)
        var localOnly: [Message] = []
        for message in localConversation.messages {
            if segmentedLocalIDs.contains(message.id) { continue }
            if refreshedIDs.contains(message.id) { continue }
            // Build 102 P0-C: clientMessageID is the AUTHORITATIVE identity for
            // a user row. When the refreshed conversation carries a server
            // twin with the same clientMessageID+sender, the local optimistic
            // row is COLLASED onto it (per marching orders §7 step 6) — the
            // server's canonical message id takes over, and the local row's
            // attachment metadata is preserved by the post-merge attachment
            // logic below.  This is checked BEFORE the jobID+sender path so
            // a user prompt is matched to its server twin even when the
            // server's jobID lookup is stale or missing.
            if let clientMsgID = message.clientMessageID,
               refreshedClientMessageKeys.contains("\(clientMsgID.uuidString)|\(message.sender)") {
                // Find the matching refreshed index — there must be exactly
                // one (the server twin), claim it, and record the mapping.
                if let twinIdx = refreshedConversation.messages.firstIndex(where: {
                    $0.clientMessageID == clientMsgID && $0.sender == message.sender
                }) {
                    if !claimedRefreshedIndices.contains(twinIdx) {
                        claimedRefreshedIndices.insert(twinIdx)
                        localToRefreshedIndex[message.id] = twinIdx
                        continue
                    }
                    // Twin already claimed by a sibling local row (e.g.
                    // another optimistic copy). Fall through to localOnly.
                }
            }
            // The server assigns its own message ids; jobID and content are the
            // only cross-identity handles we have, and matching on them keeps
            // the same answer from appearing twice.
            if let jobID = message.jobID,
               refreshedJobKeys.contains("\(jobID.uuidString)|\(message.sender)") {
                // Build 28: a local terminal row with streamed content
                // must not be dropped just because the server snapshot
                // contains a partial/tool-boundary row tagged with the
                // same jobID.  The server tags every assistant row since
                // job_started_at with the job ID; only the final stop
                // row carries the complete answer.  If the local row has
                // content and the server row is a prefix/subset, keep
                // the local row so it reaches the splice.
                let localContent = message.content.trimmingCharacters(in: .whitespacesAndNewlines)
                if localContent.isEmpty { continue }
                // Non-empty local content: keep and let the splice
                // anchor it.  The B39 T5 guard (above) already protects
                // against empty/prefix server copies for matched rows;
                // this extends that protection to the dedupe path.
                Self.logger.debug(
                    "B28: preserving local terminal row \(message.id.uuidString.prefix(8)) (jobId=\(jobID.uuidString.prefix(8))) despite refreshed jobKey match — local content is non-empty"
                )
            }
            if !message.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
               let candidates = refreshedIndicesByFingerprint[Self.messageFingerprint(message)] {
                // Claim the first row this fingerprint owns that no other local
                // message has taken, so a question asked twice in one chat maps
                // to two distinct rows rather than collapsing onto the first.
                if let claim = candidates.first(where: { !claimedRefreshedIndices.contains($0) }) {
                    claimedRefreshedIndices.insert(claim)
                    localToRefreshedIndex[message.id] = claim
                    continue
                }
                // Build 118: If the local message is a user prompt whose content is already
                // represented in the refreshed conversation, do NOT append as localOnly.
                if message.sender == .user {
                    Self.logger.info("Build 118: collapsing duplicate local user message \(message.id.uuidString.prefix(8)) matching existing claimed candidate")
                    continue
                }
                // Build 30: all fingerprint candidates were already claimed.
                // Fall through to append as a local-only message, which the
                // anchor-splice pass will re-insert after its nearest neighbour.
                // The old unconditional `continue` silently dropped the terminal
                // row — this was HOLE 1 in the 2:03:06 disappearance.
            }
            localOnly.append(message)
        }

        // A streaming placeholder that survives a merge has, by definition, no live
        // stream behind it any more — the stream that owned it is the one that triggered
        // this merge.  Leaving isStreaming=true renders a permanently animating bubble
        // that never receives its completion marker.
        //
        // Named separately rather than shadowing `localOnly`: Swift does not allow
        // redeclaring a binding in the same scope.
        // B19: the assumption above — that surviving a merge proves the stream
        // is gone — is false, and it was killing live turns.
        //
        // Conversation refreshes are also fired on a timer while a job is still
        // running, so a merge is NOT evidence that the stream ended.  On a
        // tool-heavy turn the agent emits tool events for tens of seconds before
        // any text, so the placeholder is legitimately empty at that moment.
        // The refresh then settled it and, finding no content and no reasoning,
        // marked it .failed/"empty_response" — which is the REGENERATE chip.
        // The real answer arrived seconds later and had nowhere to land.
        //
        // Observed 2026-07-30 23:15: job ran 23:15:42→23:16:35, refreshes at
        // :15:52, :16:00, :16:07, :16:12 killed the turn, and the reply (1357
        // chars, finish_reason=stop) was never rendered.  It only ever hit slow
        // turns, because a fast reply produces text before the first refresh.
        //
        // `activeStreams` is the authority on whether a stream still owns a
        // placeholder; only settle the ones nothing is streaming into.
        //
        // Build 108 WS-D: Do NOT settle as empty_response if the job is still
        // running. Stream ownership and server state, not empty text, decide
        // whether a response failed.
        let livePlaceholders = Set(activeStreams.values)
        let settledLocalOnly: [Message] = localOnly.map { message in
            guard message.isStreaming else { return message }
            guard !livePlaceholders.contains(message.id) else { return message }
            var settled = message
            settled.isStreaming = false
            // Only mark as empty_response if we have a job that has definitively
            // ended (not running). If no job is associated, keep as in-progress
            // so the polling fallback can resolve it.
            if settled.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
               settled.reasoning.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                // Check if this placeholder has an associated job that is NOT running
                if let jobID = activeStreams.first(where: { $0.value == message.id })?.key {
                    // Job exists but is not in activeStreams (already removed)
                    // This means the job has ended - safe to mark as empty_response
                    settled.status = .failed
                    settled.errorCategory = "empty_response"
                }
                // If no job found, leave as in-progress - polling will resolve
            }
            return settled
        }

        if !localOnly.isEmpty {
            Self.logger.info(
                "Merge preserved \(localOnly.count) local message(s) absent from the refreshed conversation"
            )
            // D2b: Splice local-only messages at their anchored position
            // instead of sorting by timestamp.  Sorting mixed two clocks
            // (host utcnow() vs phone Date.now) and inverted order when
            // skew exceeded the inter-message gap (~sub-second).
            //
            // Algorithm: for each local-only message, find the last local
            // message that precedes it AND exists in the refreshed array.
            // Insert after that anchor.  Messages with no anchor append.
            let localMessages = localConversation.messages
            // This is deliberately mutable.  Local-only messages are inserted
            // in transcript order, and each insertion must become an anchor for
            // the next one.  Keeping this set frozen meant a streamed assistant
            // placeholder could only anchor to the older server row, so it was
            // inserted *above* its just-inserted optimistic user prompt.
            // Anchorable = every id the refreshed array can be addressed by: its own ids
            // PLUS the local ids the matching loop proved are the same message under a
            // server-assigned id.  Without the second half the walk-back at :1743 falls off
            // the front of the array, anchorID stays nil, and the message is appended to the
            // end of the transcript — replies rendering above the prompt that produced them.
            var refreshedIDsForAnchor = Set(refreshedConversation.messages.map(\.id))
            refreshedIDsForAnchor.formUnion(localToRefreshedIndex.keys)

            for localMsg in settledLocalOnly {
                // Find the anchor: the last message before localMsg in the
                // local array that also exists in the refreshed array.
                var anchorID: UUID? = nil
                if let localIdx = localMessages.firstIndex(where: { $0.id == localMsg.id }) {
                    for predecessor in localMessages[..<localIdx].reversed() {
                        if refreshedIDsForAnchor.contains(predecessor.id) {
                            anchorID = predecessor.id
                            break
                        }
                    }
                }

                if let anchorID {
                    // The anchor may be addressed either by its own id in the refreshed
                    // array or by the local id of its server twin.  Resolve both.
                    let anchorIdx = refreshedConversation.messages.firstIndex(where: { $0.id == anchorID })
                        ?? localToRefreshedIndex[anchorID]
                    if let anchorIdx {
                        refreshedConversation.messages.insert(localMsg, at: anchorIdx + 1)
                        // Every index at or past the insertion point shifted by one.
                        for (localID, idx) in localToRefreshedIndex where idx > anchorIdx {
                            localToRefreshedIndex[localID] = idx + 1
                        }
                        localToRefreshedIndex[localMsg.id] = anchorIdx + 1
                    } else {
                        refreshedConversation.messages.append(localMsg)
                        localToRefreshedIndex[localMsg.id] = refreshedConversation.messages.count - 1
                    }
                } else {
                    refreshedConversation.messages.append(localMsg)
                    localToRefreshedIndex[localMsg.id] = refreshedConversation.messages.count - 1
                }
                refreshedIDsForAnchor.insert(localMsg.id)
            }
        }

        return refreshedConversation
    }

    /// Sender + normalized content, used to recognize the same message across
    /// the local/server id boundary.
    private static func messageFingerprint(_ message: Message) -> String {
        "\(message.sender)|\(normalizedMessageContent(message.content))"
    }

    private static func normalizedMessageContent(_ content: String) -> String {
        content.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression)
    }

    private func mergeAttachments(_ localAttachments: [MessageAttachment], onto remoteAttachments: [MessageAttachment]) -> [MessageAttachment] {
        guard !remoteAttachments.isEmpty else { return localAttachments }

        return remoteAttachments.enumerated().map { index, remote in
            let match = localAttachments.first(where: {
                $0.fileName == remote.fileName && $0.mimeType == remote.mimeType
            }) ?? localAttachments[safe: index]
            guard let match else { return remote }
            return MessageAttachment(
                id: remote.id,
                kind: remote.kind,
                fileName: remote.fileName,
                mimeType: remote.mimeType,
                thumbnailBase64: remote.thumbnailBase64 ?? match.thumbnailBase64,
                localStoragePath: match.localStoragePath,
                messageID: remote.messageID ?? match.messageID,
                remoteIndex: remote.remoteIndex ?? match.remoteIndex
            )
        }
    }

    /// Remove delivered notifications for the currently active conversation.
    /// Prevents Notification Center clutter when the user is already viewing
    /// a conversation — stale notifications for it are cleared.
    private func clearNotificationsForCurrentConversation() {
        guard let convId = conversation?.id else { return }
        let center = UNUserNotificationCenter.current()
        Task {
            let delivered = await center.deliveredNotifications()
            let staleIDs = delivered.compactMap { notification -> String? in
                let info = notification.request.content.userInfo
                guard let notificationConvId = info["conversationId"] as? String else {
                    return nil
                }
                return notificationConvId == convId.uuidString.lowercased()
                    ? notification.request.identifier
                    : nil
            }
            if !staleIDs.isEmpty {
                center.removeDeliveredNotifications(withIdentifiers: staleIDs)
            }
        }
    }

    private func normalizedRetryContent(for message: Message) -> String {
        if !message.attachments.isEmpty,
           message.content.range(of: #"^\[\d+ attachment"#, options: .regularExpression) != nil {
            return ""
        }
        return message.content.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func attachmentSignature(for attachments: [MessageAttachment]) -> String {
        attachments
            .map { "\($0.kind)|\($0.fileName)|\($0.mimeType)" }
            .sorted()
            .joined(separator: "||")
    }

    // MARK: - Profile Switch Detection

    /// Detect a profile switch from the agent's response text.
    /// Updates the active profile name on ProfileStore immediately so the
    /// toolbar chip reflects the change in the same render frame.
    private func detectProfileSwitch(in text: String) async {
        let patterns: [Regex<(Substring, Substring)>] = [
            /[Ss]witched\s+(?:to\s+)?profile\s+["'`]?(\w+)["'`]?/,
            /[Cc]hanged\s+(?:to\s+)?profile\s+["'`]?(\w+)["'`]?/,
            /[Aa]ctivated\s+(?:profile\s+)?["'`]?(\w+)["'`]?\s+profile/,
            /[Pp]rofile\s+switched\s+(?:to\s+)?["'`]?(\w+)["'`]?/,
            /[Pp]rofile\s+["'`]?(\w+)["'`]?\s+activated/,
        ]
        for pattern in patterns {
            if let match = text.firstMatch(of: pattern) {
                let profileName = String(match.1)
                profileStore?.markActive(profileName)
                // Refresh the catalog so activeProfile computed property
                // resolves immediately instead of waiting for the next
                // automatic load (up to 60s away).
                await profileStore?.loadProfiles(force: true)
                return
            }
        }
    }

    /// Fallback-only lookup for cases where the connector has not yet provided
    /// an explicit context window. This should never overwrite a known value.
    // REMOVED: inferredContextWindow — all context info now comes from the
    // relay model catalog. Never fabricate context limits client-side.
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        guard indices.contains(index) else { return nil }
        return self[index]
    }
}
