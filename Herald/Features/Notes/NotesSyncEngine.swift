import Foundation
import os
import PencilKit
import UIKit

/// Syncs notes to the gateway as sessions (Option 1 architecture):
/// - Each note maps to a real gateway session titled with the note's name.
/// - The first sync creates the session; every later edit appends a NEW
///   message to that same session, so the note thread grows like a chat.
/// - Sync cadence comes from `SettingsStore.settings.notesSyncInterval`
///   (2m..24h or manual), and a manual sync is always available.
/// - Progress is observable so the UI can show a realtime status bar and
///   what the background sync is doing right now.
@MainActor
@Observable
final class NotesSyncEngine {
    enum Stage: Equatable {
        case idle
        case preparing
        case creatingSession
        case uploading
        case sending
        case done
        case failed(String)
    }

    enum SyncTrigger: Equatable {
        case automatic
        case manual
    }

    /// One note's sync attempt. Reported to the UI as it happens.
    struct NoteSyncProgress: Equatable {
        let noteTitle: String
        let index: Int
        let total: Int
    }

    // MARK: - Observable state

    private(set) var stage: Stage = .idle
    private(set) var currentProgress: NoteSyncProgress?
    private(set) var lastSyncDate: Date?
    private(set) var lastSyncError: String?
    private(set) var isSyncing = false

    /// The agent's live reasoning stream from the gateway while a note syncs -
    /// rendered with the SAME ReasoningView component chat uses. Updated by the
    /// sync engine from `reasoning.delta` events; the notes UI consumes it.
    private(set) var liveReasoning: String = ""
    private(set) var isReasoningActive = false
    private(set) var reasoningStartedAt: Date?
    private(set) var reasoningDuration: TimeInterval?

    /// Notes fix (Build N): once a sync's reasoning stream completes, freeze a
    /// snapshot here so the notes UI can render a collapsible "Thought for Xs"
    /// card that survives the live-stream reset on the next sync. Cleared
    /// explicitly via `dismissCompletedReasoning()`.
    private(set) var lastCompletedReasoning: String = ""
    private(set) var lastCompletedAt: Date?
    private(set) var lastCompletedDuration: TimeInterval?

    /// Current sync's transition into its terminal stage. The editor bubbles
    /// up `[Notes sync] <stage label>` from the moment `isSyncing` becomes
    /// true so a `reasoning.delta`-less turn still has visible liveness.
    /// Falls back to `statusText` when empty.
    var liveStageLabel: String {
        switch stage {
        case .idle:
            return lastSyncError ?? "Idle"
        case .preparing:
            return "Preparing sync"
        case .creatingSession:
            return "Preparing enrichment"
        case .uploading:
            return "Uploading drawing"
        case .sending:
            if let currentProgress {
                return "Sending \\(currentProgress.noteTitle) (\(currentProgress.index)/\(currentProgress.total))"
            }
            return "Sending"
        case .done:
            return "Synced"
        case .failed(let message):
            return "Sync failed: \(message)"
        }
    }

    /// Label describing what the engine is doing right now (for the progress bar).
    var statusText: String {
        switch stage {
        case .idle:
            return "Sync idle"
        case .preparing:
            return "Preparing notes..."
        case .creatingSession:
            return "Preparing enrichment..."
        case .uploading:
            return "Uploading drawing..."
        case .sending:
            if let currentProgress {
                return "Sending \"\(currentProgress.noteTitle)\" (\(currentProgress.index)/\(currentProgress.total))..."
            }
            return "Sending..."
        case .done:
            return "Notes synced"
        case .failed(let message):
            return "Sync failed: \(message)"
        }
    }

    var hasPendingSync: Bool {
        pendingNotesCount > 0
    }

    /// Number of notes dirty since the last sync (changed drawing or never synced).
    private var pendingNotesCount: Int = 0

    // MARK: - Dependencies

    private let notesStore: NotesStore
    private let settingsStore: SettingsStore
    /// Resolves the active chat client at call time (native gateway or relay).
    private let clientProvider: () async -> (any HeraldClientProtocol)?
    private let logger = Logger(subsystem: "net.fihonline.kallisti", category: "notes-sync")

    private var timerTask: Task<Void, Never>?
    private var syncTask: Task<Void, Never>?

    init(
        notesStore: NotesStore,
        settingsStore: SettingsStore,
        clientProvider: @escaping () async -> (any HeraldClientProtocol)?
    ) {
        self.notesStore = notesStore
        self.settingsStore = settingsStore
        self.clientProvider = clientProvider
    }

    // MARK: - Lifecycle

    /// Start the automatic cadence timer. Safe to call multiple times; the
    /// loop re-reads the interval each cycle so setting changes apply live.
    func start() {
        guard timerTask == nil else { return }
        timerTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                // Re-read the interval every cycle from live settings.
                let interval = self.settingsStore.settings.notesSyncInterval.intervalSeconds
                if let interval {
                    try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
                    if Task.isCancelled { return }
                    await self.syncIfNeeded(trigger: .automatic)
                } else {
                    // Manual mode: idle 30s at a time so a settings change
                    // (enabling auto-sync) is picked up quickly.
                    try? await Task.sleep(nanoseconds: 30_000_000_000)
                }
            }
        }
    }

    func stop() {
        timerTask?.cancel()
        timerTask = nil
    }

    // MARK: - Sync entry points

    /// Manual sync - the sync button on the notes screens. Always runs,
    /// regardless of the cadence setting.
    func syncNow() async {
        guard !isSyncing else { return }
        await syncIfNeeded(trigger: .manual)
    }

    /// Build 128.97: sync ONLY the active note (the one being viewed/edited).
    /// The sync button in the note editor must never sweep every dirty note -
    /// Curtis's rule: one note, one session, one task per press.
    func syncNote(id: UUID) async {
        guard !isSyncing else { return }
        guard let note = notesStore.activeNotes.first(where: { $0.id == id }) else {
            stage = .failed("Note not found.")
            lastSyncError = "Note not found."
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if stage == .failed(lastSyncError ?? "") { stage = .idle }
            return
        }
        isSyncing = true
        stage = .preparing
        lastSyncError = nil

        // Enrichment toggle: same guard as the batch path - OFF means notes
        // never leave the device.
        guard settingsStore.settings.notesEnrichmentEnabled else {
            stage = .failed("Enrichment is disabled in Settings > Notes Sync.")
            lastSyncError = "Enrichment is disabled in Settings > Notes Sync."
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if stage == .failed(lastSyncError ?? "") { stage = .idle }
            isSyncing = false
            return
        }

        // Skip-when-no-update: a clean note is a true no-op even on a manual
        // press (dirty check identical to the batch filter).
        if !isDirty(note) {
            stage = .done
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if stage == .done { stage = .idle }
            lastSyncDate = .now
            logger.info("Manual sync: note \\(note.title) has no changes - skipped")
            isSyncing = false
            return
        }

        guard let client = await clientProvider() else {
            stage = .failed("Not connected. Connect to the gateway first.")
            lastSyncError = "Not connected. Connect to the gateway first."
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if stage == .failed(lastSyncError ?? "") { stage = .idle }
            isSyncing = false
            return
        }

        pendingNotesCount = 1
        currentProgress = NoteSyncProgress(noteTitle: note.title, index: 1, total: 1)
        do {
            try await syncSingleNote(note, client: client)
            stage = .done
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if stage == .done { stage = .idle }
        } catch {
            stage = .failed(error.localizedDescription)
            lastSyncError = error.localizedDescription
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if stage == .failed(lastSyncError ?? "") { stage = .idle }
        }
        currentProgress = nil
        lastSyncDate = .now
        isSyncing = false
    }

    /// Sync every note that changed since its last sync. Respects the
    /// configured interval when called from the timer.
    func syncIfNeeded(trigger: SyncTrigger = .automatic) async {
        guard !isSyncing else { return }
        isSyncing = true
        stage = .preparing
        lastSyncError = nil

        // Build 128.94: enrichment toggle. When OFF, notes never leave the
        // device - no sessions, no model turns. A manual tap reports why it
        // did nothing so the UI doesn't look broken; the timer just idles.
        guard settingsStore.settings.notesEnrichmentEnabled else {
            if trigger == .manual {
                stage = .failed("Enrichment is disabled in Settings > Notes Sync.")
                lastSyncError = "Enrichment is disabled in Settings > Notes Sync."
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                if stage == .failed(lastSyncError ?? "") { stage = .idle }
            } else {
                stage = .idle
            }
            lastSyncDate = .now
            logger.info("Sync run skipped: note enrichment is disabled")
            isSyncing = false
            return
        }

        // Gather dirty notes (Build 128.73): only REAL activity syncs.
        // A blank or untouched note is skipped so a run with zero changes is
        // a true no-op: drawing changed since the last push, note touched
        // since the last sync (title/style/text), or new content that was
        // never pushed.
        let notes = notesStore.activeNotes
        let dirty = notes.filter(isDirty)
        pendingNotesCount = dirty.count
        // Never leave a silent no-op: a manual tap with nothing to push
        // should still report back so it doesn't look broken.
        if dirty.isEmpty {
            if trigger == .manual {
                stage = .done
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                if stage == .done { stage = .idle }
            } else {
                stage = .idle
            }
            lastSyncDate = .now
            logger.info("Sync run found no dirty notes (\(notes.count) active)")
            isSyncing = false
            return
        }

        // Resolve the chat client once for this pass.
        guard let client = await clientProvider() else {
            stage = .failed("Not connected. Connect to the gateway first.")
            lastSyncError = "Not connected. Connect to the gateway first."
            isSyncing = false
            return
        }

        var synced = 0
        var firstError: String?
        for (index, note) in dirty.enumerated() {
            if Task.isCancelled { break }
            currentProgress = NoteSyncProgress(noteTitle: note.title.isEmpty ? "Untitled" : note.title, index: index + 1, total: dirty.count)
            do {
                try await syncSingleNote(note, client: client)
                synced += 1
            } catch {
                logger.error("Sync failed for note \(note.id): \(error.localizedDescription)")
                if firstError == nil { firstError = error.localizedDescription }
            }
        }

        currentProgress = nil
        pendingNotesCount = 0
        if let firstError {
            stage = .failed(firstError)
            lastSyncError = firstError
        } else {
            stage = .done
        }
        lastSyncDate = .now
        // Keep the done state visible briefly, then idle.
        try? await Task.sleep(nanoseconds: 2_500_000_000)
        if stage == .done { stage = .idle }
        isSyncing = false
    }

    // MARK: - Single note

    /// Grounding contract for every automatic note enrichment. Keep this separately
    /// testable: a thin visual note must never turn into invented research.
    /// Build 135.39: rewritten as a turn contract. Verified failure modes from
    /// 2026-08-29 UAT: (1) no-model-identity -> tool discovery spirals that
    /// burned 60-75s of the turn budget; (2) greedy JSON sections -> invalid
    /// output silently dropped by the Enriched tab; (3) no completion signal ->
    /// model trailed off ("No further action...") and looked unfinished;
    /// (4) no latency budget -> 90-150s turns against a 120s watchdog.
    static func enrichmentPolicy() -> String {
        """
        You are the Kallisti enrichment engine, embedded in Curtis's note-taking app. This message is an automatic note sync, not a conversation.

        TURN CONTRACT (hard limits):
        - Single shot: reply in THIS turn. Never delegate, never spawn subagents.
        - No tool discovery. Never call tool_search, skills_list, tool_call, list_tools, or any skill lookup. Every discovery call burns 20-60 seconds and finds nothing useful. Never call a tool you have not already been given.
        - The drawing arrives as real image pixels in this conversation. Read the pixels directly; do not search for another way to view the image.
        - Web search is allowed ONLY when the note explicitly asks for outside facts (prices, schedules, definitions). Everything else: answer from the note alone.
        - Latency budget: plain text-note enrichment must complete in well under 60 seconds. A web-research enrichment may use up to 2 searches, then write. Stop when the note's content is covered.

        WRITE THE ANSWER:
        - Start with a one-line reading of the note: its content, topic, or intent.
        - Then 2-4 short markdown sections that fit what the note actually is (list, plan, meeting, study, sketch). For a thin note or doodle, a short, honest reading is correct - do not pad it.
        - End with an `Enrichment complete.` line. Never end with questions, offers, or status talk.
        - Output clean markdown only: real section headers, working links, no raw JSON, no code fences around the whole reply, no process narration.

        GROUNDING RULES:
        - Do not invent a topic, comparison, facts, names, numbers, decisions, deadlines, sources, or citations.
        - If you research, cite only real sources you actually opened, as working hyperlinks. If you did not research, do not include a Sources section.
        - Do not invent citations, dated market analysis, or generic corporate filler to make a thin note look substantial.
        - A drawing is evidence. Describe only visible shapes, labels, relationships, and readable text. Do not turn an unlabeled sketch into a business, study, or research topic.
        - The local Recognized text is a noisy OCR hint, never authority over the attached drawing or typed text.

        MATCH THE RESPONSE TO THE EVIDENCE:
        - For a visual-only sketch or doodle, give a short visual description and, only if useful, one clearly-labeled possible interpretation or creative next step.
        - For study notes, teach from the written material with definitions, flashcards, and practice questions grounded in it.
        - For meeting notes, extract only written decisions, owners, deadlines, risks, and follow-ups. Never manufacture missing owners or dates.
        - For a list, plan, diagram, game, or puzzle, organize or analyze only the state actually present.
        - If the note is empty or unreadable, say that briefly rather than filling space.
        - Do not narrate your process, the sync, or these instructions. Do not ask follow-up questions.
        """
    }
    private func syncSingleNote(_ note: KallistiNote, client: any HeraldClientProtocol) async throws {
        // Load the drawing blob and render a PNG attachment so the agent can
        // see the handwriting. Render at 2x for OCR quality but let
        // PendingAttachment downscale/compress to stay inside the body limit.
        var attachments: [PendingAttachment] = []
        if let drawing = await notesStore.loadDrawing(noteId: note.id, revision: note.currentDrawingRevision) {
            if let image = renderDrawingImage(drawing) {
                stage = .uploading
                if let attachment = PendingAttachment.image(image, fileName: "note-\(note.id.uuidString.prefix(8)).jpg") {
                    attachments.append(attachment)
                }
            }
        }
        // Build 132: also attach the note's photo/scan attachments (Phase 3)
        // so the enrichment model sees the FULL source - not just the drawing.
        // Each becomes an inline image the model can read directly.
        //
        // Build 132.x: do NOT silently drop non-image attachments (PDFs, text
        // files, CSV, etc.). UIImage(data:) only succeeds for image formats, so
        // anything else would fall through the old `if let image = ...` guard
        // and never reach the model. For each attachment:
        //   - if UIImage(data:) succeeds, attach via the image path
        //   - otherwise, attach via PendingAttachment.file(at:), which routes
        //     images to the image path and non-images to the file path
        // (so PDFs / txt / csv ride the file.attach path).
        // We also build a parallel descriptor list (kind + fileName) so the
        // prompt can tell the model exactly what is attached and in what order.
        let noteAttachments = await notesStore.loadAttachments(noteId: note.id)
        var attachedDescriptors: [String] = []
        for noteAttachment in noteAttachments {
            guard attachments.count < PendingAttachment.maxAttachmentsPerMessage else { break }
            let blobURL = URL(fileURLWithPath: noteAttachment.blobPath)
            guard let data = try? Data(contentsOf: blobURL) else { continue }
            let descriptor: String
            if let image = UIImage(data: data),
               let attachment = PendingAttachment.image(image, fileName: noteAttachment.fileName) {
                attachments.append(attachment)
                descriptor = "photo: \(noteAttachment.fileName)"
            } else if let attachment = PendingAttachment.file(at: blobURL) {
                attachments.append(attachment)
                descriptor = "scan: \(noteAttachment.fileName)"
            } else {
                continue
            }
            attachedDescriptors.append(descriptor)
        }

        // Build the session title from the note's current title.
        let sessionTitle = note.title.isEmpty ? "Untitled Note" : note.title

        // Build 128.94: SMART enrichment prompt. No more handcuffing - the old
        // prompt told the model "No drawing attached - this note has no
        // drawable content. Reply briefly." and forced a narrow OCR+summarize
        // task. The app already runs local OCR; the model should ACT on what
        // it sees: if the note is new, create a full enrichment; if it is an
        // update, treat this as an additional message to the note's session
        // and update the enrichment accordingly. The drawing image is attached
        // when present so vision-capable models can read handwriting directly.
        // The enrichment model/provider is named in the prompt so the model
        // knows what is handling the note.
        let isNewNote = note.lastSyncedAt == nil
        // Build 135.31 (smart-title fix): the note's real content leads so
        // the gateway auto-titler derives from it, not the machine envelope.
        // The "[Note sync from Kallisti...]" marker is appended later (after
        // typed text) so it no longer poisons the derived title.
        var messageText = "Note title: \(sessionTitle)\n"
        if isNewNote {
            messageText += "This is a NEW note - create the enrichment for it.\n"
        } else {
            messageText += "This is an UPDATE to an existing note - treat this as an additional message to this note's session and update the enrichment accordingly.\n"
        }

        // Include the locally-recognized text when it exists so the model
        // works from the real content (the app OCRs on-device with Vision).
        if let recognitionText = await loadLatestRecognitionText(for: note) {
            messageText += "Recognized text: \"\(recognitionText)\"\n"
        }

        // Include the typed-text body when it exists (Build 131: notes can
        // now carry keyboard-typed text alongside ink).
        if let typedText = await notesStore.loadTypedText(noteId: note.id),
           !typedText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            messageText += "Typed text: \"\(typedText)\"\n"
        }

        // Build 135.31 (smart-title fix): append the machine envelope here,
        // AFTER the human content, so the gateway auto-titler's leading-line
        // derivation reads real content first.
        messageText += "[Note sync from Kallisti - automatic, not a chat message]\n"

        // Enrichment provider context - the model knows who it is.
        let settings = settingsStore.settings
        if let modelName = settings.notesEnrichmentModelName, !modelName.isEmpty {
            let provider = settings.notesEnrichmentProvider ?? ""
            if provider.isEmpty {
                messageText += "Enrichment provider: \(modelName)\n"
            } else {
                messageText += "Enrichment provider: \(provider)/\(modelName)\n"
            }
        }

        if attachments.isEmpty {
            messageText += "No drawing attached - use the recognized text above. If the note is empty, say so briefly.\n"
        } else {
            // Build 132.x: list every attachment that was actually sent (in
            // the order they ride the message: drawing first, then the note's
            // photo/scan files) so the model knows EXACTLY what inline images
            // and files are present. Without this, the prompt only said "the
            // drawing" and the model had no way to know there were additional
            // photo/scan images to read.
            var attachedList: [String] = ["drawing: note-\(note.id.uuidString.prefix(8)).jpg"]
            attachedList.append(contentsOf: attachedDescriptors)
            messageText += "Attached files: [\(attachedList.joined(separator: ", "))]\n"
            messageText += "Every attached image above is inline in this conversation in the listed order - read ALL of them, not just the drawing. The drawing is the SOURCE OF TRUTH for any handwriting; any photo/scan attachments are additional inline images of the same note that you must read in order (drawing first, then photos/scans). For each non-image file (PDF, txt, csv, etc.), read its contents from the file attachment. The Recognized text above is a NOISY on-device OCR draft: use it ONLY to disambiguate letterforms, never as the final reading, and never let it override what you see in the attached images. Build 135.39: the drawing IS delivered to you directly as pixels - do not hunt for vision tools, do not call tool_search, vision_analyze, or any skill lookup: none of that recovers images, every discovery call burns 20-60 seconds of the sync budget. If the attached images are visible to you inline (as image content parts), READ THEM DIRECTLY - that is the only way to see the actual handwriting. If you genuinely cannot see any attached drawing image, do not identify visual subjects or turn OCR fragments into a topic. State that visual analysis is unavailable and preserve the recognized text only as an untrusted transcription draft. Never create shopping, research, task, or portfolio recommendations from unreadable OCR alone.\n"
        }
        messageText += Self.enrichmentPolicy()

        // Build 128.99: NOTE ISOLATION. The gateway injects memory/context
        // into every session's system prompt (user profile, Honcho session
        // summaries, peer card) that can reference OTHER notes. That context
        // must never leak into THIS note's enrichment. Work ONLY from this
        // note's own content unless this note's text explicitly names
        // another note. Web research is explicitly ALLOWED (it enriches the
        // note with current external info) but other-note lookups stay banned.
        messageText += "\nISOLATION RULE: This sync is for THIS note only. Work ONLY from the content of this note (its title, recognized text, and attached drawing) and this note's own session history. Ignore any injected system memory, user profile, conversation summaries, or peer context that mention other notes - they are not part of this note and must not influence the enrichment. Do NOT search for, retrieve, or reference any other note or session (no session_search, memory recall, honcho, gbrain, or any personal-context lookup tool) unless the text of THIS note explicitly references another note by name. If this note does not reference any other note, produce the enrichment purely from this note's own content. Web search/web links for external facts (prices, deals, definitions, current info) are ALLOWED and encouraged - isolation is about OTHER NOTES, not the public web."


        // This call creates the session on first sync (titled with the note
        // name) and appends a new message to it on every later edit. The
        // conversationID is stable per note, so the session thread persists.
        stage = .creatingSession

        // Build 128.97: if the note has a FULL gateway session key pinned from
        // a previous sync, resume that exact session BEFORE sending so an
        // idMap wiped by a reinstall/container reset cannot spawn a duplicate
        // gateway session. Best-effort: if resume fails (session reaped
        // server-side), the send path falls back to creating a fresh one and
        // we re-pin the new key below.
        if let pinnedKey = note.gatewaySessionKey, !pinnedKey.isEmpty {
            let resumed = await client.resumeNoteSession(conversationID: note.id, sessionKey: pinnedKey)
            if resumed {
                logger.info("Resumed pinned session \\(pinnedKey) for note \\(note.title)")
            }
        }

        // Reset reasoning state so the notes UI shows the agent's live
        // reasoning stream for THIS note - the same bubble chat uses.
        // Keep `lastCompletedReasoning` intact: the collapsible card from the
        // previous sync stays visible until the user explicitly dismisses it
        // OR a new completion overwrites it at success-time below.
        liveReasoning = ""
        isReasoningActive = true
        reasoningStartedAt = .now
        reasoningDuration = nil

        // Run the stream consumer in a child Task so the watchdog can truly
        // cancel it. A bare for-await over AsyncStream blocks forever if the
        // gateway goes silent; cancelling the consuming task terminates the
        // iteration, so the sync can never hang at "Creating session..." again.
        let consumeTask = Task { () -> (Message?, String?) in
            var finalContent = ""
            var message: Message?
            var streamError: String?
            for await update in client.sendNoteMessageStreaming(
                text: messageText,
                attachments: attachments,
                clientMessageID: UUID(),
                conversationID: note.id,
                title: sessionTitle,
                enrichmentModelName: settingsStore.settings.notesEnrichmentModelName,
                enrichmentProvider: settingsStore.settings.notesEnrichmentProvider,
                thinkingAsReasoning: true
            ) {
                if Task.isCancelled { streamError = "Sync cancelled."; break }
                switch update {
                case .textDelta(let delta):
                    finalContent += delta
                case .reasoningDelta(let delta):
                    liveReasoning += delta
                case .toolStarted(let activity):
                    // Build 135.39: surface tool phases as reasoning lines so a
                    // long tool run is visible instead of a frozen bubble. The
                    // consumer previously listened for `.toolActivity`, which
                    // the native client stopped yielding when tool lifecycle
                    // events (.toolStarted/.toolCompleted) landed - notes tool
                    // progress had been dead since then.
                    if !liveReasoning.isEmpty { liveReasoning += "\n" }
                    liveReasoning += "[tool] \(activity.label)"
                case .toolCompleted(let toolCallID, _, _, _):
                    // Keep the reasoning card current when a tool run finishes;
                    // ignore the id - the next [tool] line replaces this one.
                    _ = toolCallID
                    if !liveReasoning.isEmpty { liveReasoning += "\n" }
                    liveReasoning += "[tool] done"
                case .finished(let msg, _, _, _):
                    message = msg
                case .failed(let error, _, _):
                    streamError = error
                default:
                    break
                }
                if message != nil || streamError != nil { break }
            }
            return (message, streamError)
        }

        let watchdogTask = Task {
            // A sync must never hang silently. If the gateway streams nothing
            // terminal, cancel the consumer (terminates the for-await) so the
            // engine moves on to a clear failure.
            // Build 135.39: 120s -> 180s. Real note turns measured 86.9s and
            // 150.6s server-side (2026-08-29 agent.log) while this watchdog
            // killed the stream at 120s - the model's finished reply never
            // reached the Enriched tab ("No Enrichment Yet"). 180s covers a
            // 2-call web-research turn with headroom.
            try? await Task.sleep(nanoseconds: 180_000_000_000)
            consumeTask.cancel()
        }
        let (message, streamError) = await consumeTask.value
        watchdogTask.cancel()

        isReasoningActive = false
        if let startedAt = reasoningStartedAt {
            reasoningDuration = Date.now.timeIntervalSince(startedAt)
        }

        // Notes fix: snapshot the live stream into the completed-card slot so
        // a collapsible "Thought for Xs" survives the reset. Only succeed
        // when the consumer actually saw real reasoning OR the user just
        // wants the timing - a totally empty stream leaves the prior card
        // untouched (no flicker on bookkeeping-only turns).
        if !liveReasoning.isEmpty {
            lastCompletedReasoning = liveReasoning
            lastCompletedAt = .now
            lastCompletedDuration = reasoningDuration
        }

        if let streamError {
            throw NotesSyncError.sendFailed(streamError)
        }
        guard let message else {
            throw NotesSyncError.sendFailed("Gateway closed the stream before completing.")
        }
        if message.status == .failed || message.content.hasPrefix("Note sync error") || message.content.hasPrefix("Note sync requires") {
            throw NotesSyncError.sendFailed(message.content)
        }

        // Persist the sync checkpoint on the note so we don't re-send the
        // same revision next cycle. Build 128.97 also pins the FULL gateway
        // session key on the note (survives reinstall/container reset so the
        // same session is resumed next time, never a duplicate) and saves the
        // model's reply as the note's enrichment result (this is what fills
        // the Enrichment tab).
        if var stored = notesStore.notes.first(where: { $0.id == note.id }) {
            stored.lastSyncedDrawingRevision = note.currentDrawingRevision
            stored.lastSyncedAt = .now
            stored.syncState = .synced
            if let sessionKey = await client.nativeSessionKey(for: note.id), !sessionKey.isEmpty {
                stored.gatewaySessionKey = sessionKey
            }

            // Build 130.1: SMART TITLE for untitled notes. If the note has
            // no real title after a successful sync (manual or auto), name
            // it using Hermes' NATIVE title generation - the same
            // llm.oneshot task=title_generation path the gateway uses for
            // chat sessions - then rename the gateway session and persist
            // the new title. The recognized text is the best stand-in for
            // the user's "opening message" of the note.
            let trimmedTitle = stored.title.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmedTitle.isEmpty || trimmedTitle == "Untitled Note" {
                if let recognitionText = await loadLatestRecognitionText(for: note) {
                    // Build 135.31 (smart-title fix): prefer the enrichment
                    // reply (the real drawing reading) over the noisy OCR draft.
                    let enriched = message.content.trimmingCharacters(in: .whitespacesAndNewlines)
                    let sourceText = enriched.isEmpty ? recognitionText : message.content
                    do {
                        let generated = try await client.generateSessionTitle(
                            sessionId: note.id,
                            userMessage: sourceText,
                            assistantMessage: message.content
                        )
                        let clean = generated.trimmingCharacters(in: .whitespacesAndNewlines)
                        if !clean.isEmpty {
                            stored.title = clean
                            // Rename the gateway session so the session list
                            // reflects the note's new name too. Best-effort.
                            _ = try? await client.renameSession(id: note.id, title: clean)
                        }
                    } catch {
                        logger.warning("Smart title generation failed for note \\(note.id): \\(error.localizedDescription)")
                    }
                }
            }

            await notesStore.updateNote(stored)

            // Save the enrichment result so the Enrichment tab renders the
            // summary board (timestamps, links, docs, attachments in order).
            let enrichment = EnrichmentResult(
                noteId: note.id,
                runId: UUID(),
                sourceDrawingRevision: note.currentDrawingRevision,
                sourceTextRevision: note.currentTextRevision,
                title: sessionTitle,
                markdown: message.content,
                createdAt: .now
            )
            let repo = NotesRepository()
            try? await repo.saveEnrichmentResult(enrichment, noteId: note.id)
        }
    }

    /// Render a PKDrawing blob to a UIImage for attachment.
    /// Build 133.1: render at 4x to match NoteRecognitionCoordinator's OCR
    /// resolution (it renders at 4.0). The enrichment model was seeing a 2x
    /// image while the on-device OCR read a 4x image - cursive strokes lost
    /// detail in the attachment, which degraded the vision-model reading.
    /// PendingAttachment downscales to fit the body limit, so 4x is safe.
    private func renderDrawingImage(_ data: Data) -> UIImage? {
        guard let drawing = try? PKDrawing(data: data) else { return nil }
        let bounds = drawing.bounds
        guard !bounds.isEmpty, bounds.width > 0, bounds.height > 0 else { return nil }
        let image = drawing.image(from: bounds, scale: 4.0)
        return image
    }

    /// Load the latest locally-OCR'd text for a note (Vision runs on-device).
    /// The sync prompt includes this so the model works from real content even
    /// when no drawing attachment is present, and can cross-check handwriting.
    private func loadLatestRecognitionText(for note: KallistiNote) async -> String? {
        let repo = NotesRepository()
        guard let recognitions = try? await repo.loadRecognitions(noteId: note.id),
              let latest = recognitions.last else { return nil }
        let text = latest.effectiveText.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }

    /// Build 128.97: dirty filter shared by the batch auto-sync and the
    /// active-note manual sync. A note needs pushing when its drawing
    /// advanced past the last sync, it was touched after the last sync, or
    /// it is a new note with any real content.
    private func isDirty(_ note: KallistiNote) -> Bool {
        if note.lastSyncedDrawingRevision < note.currentDrawingRevision {
            return true
        }
        if let lastSynced = note.lastSyncedAt, note.updatedAt > lastSynced {
            return true
        }
        if note.lastSyncedAt == nil {
            return note.currentDrawingRevision > 0
                || note.currentTextRevision > 0
                || !note.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
        return false
    }

    // MARK: - Reasoning bubble reset

    /// Clear the live reasoning stream so the notes page does not render a
    /// 'Thought for Xs' bubble left over from a previous note. Mirrors the
    /// reset performed at the start of syncSingleNote so callers that load
    /// an editor without immediately syncing still get a clean bubble.
    func resetReasoningState() {
        liveReasoning = ""
        isReasoningActive = false
        reasoningStartedAt = nil
        reasoningDuration = nil
    }

    /// Notes fix: collapse/dismiss the persisted "Thought for Xs" card from
    /// the previous sync. Called by the UI when the user taps the chevron
    /// to collapse an already-completed card so it stops showing.
    func dismissCompletedReasoning() {
        lastCompletedReasoning = ""
        lastCompletedAt = nil
        lastCompletedDuration = nil
    }
}

enum NotesSyncError: LocalizedError {
    case sendFailed(String)

    var errorDescription: String? {
        switch self {
        case .sendFailed(let message): return message
        }
    }
}
