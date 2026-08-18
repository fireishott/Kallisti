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

    /// Label describing what the engine is doing right now (for the progress bar).
    var statusText: String {
        switch stage {
        case .idle:
            return "Sync idle"
        case .preparing:
            return "Preparing notes..."
        case .creatingSession:
            return "Creating session..."
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
        var messageText = "[Note sync from Kallisti - automatic, not a chat message]\n"
        messageText += "Note title: \(sessionTitle)\n"
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
            messageText += "A drawing is attached inline as an image in this conversation. If you support vision, you can see it directly - no tool call is required or possible. Do NOT call vision_analyze or any other vision/image tool; none exists in your toolset. Do NOT search for vision tools (no tool_search, no list_tools, no discovery for vision). If you cannot see the attached image, use the Recognized text above as the authoritative transcription of the handwriting.\n"
        }
        // Build 130.0: NATURAL DIGEST output contract. Curtis's 129.0
        // complaint: enrichment read like AI process narration ("The user
        // sent an automatic note sync... I should load the handwriting-
        // recognition skill...") instead of a useful summary of the
        // handwritten note. The model must write like a sharp assistant
        // summarizing a note for someone who hasn't seen it - lead with
        // the actual content, add genuinely useful context, and never
        // narrate its own instructions or the sync mechanics.
        messageText += "Read the note (text plus the inline drawing if attached). Apply your handwriting analysis skills (the handwriting-recognition skill: read carefully, resolve ambiguous letterforms from context, preserve math notation, flag uncertain words rather than guessing). Write a natural, useful digest of the note - the way a sharp assistant would summarize a handwritten note for someone who has not seen it:\n"
        messageText += "- Lead with what the note actually says: restate the real content (the problems, items, names, numbers) in clear language, and solve or complete what it asks where you can (answer the math, expand the shorthand, fill in context).\n"
        messageText += "- Follow with the genuinely useful context a reader would want: key points, action items, any links/docs/references/attachments in the order they appear, and anything that makes the note more valuable.\n"
        messageText += "- End with a short timestamped 'Updates' line: if this is an update to an existing note, say what changed since the prior enrichment; if new, note when it was created.\n"
        messageText += "Write plainly and specifically. Do NOT describe your instructions, the sync, the session, the isolation rule, or your own process - no meta-commentary at all. Never call vision/image tools - the drawing is already inline and any vision tool call will fail. Do not ask follow-up questions. Reply concisely but do the real work.\n"

        // Build 128.99: NOTE ISOLATION. The gateway injects memory/context
        // into every session's system prompt (user profile, Honcho session
        // summaries, peer card) that can reference OTHER notes. That context
        // must never leak into THIS note's enrichment. Work ONLY from this
        // note's own content unless this note's text explicitly names
        // another note.
        messageText += "\nISOLATION RULE: This sync is for THIS note only. Work ONLY from the content of this note (its title, recognized text, and attached drawing) and this note's own session history. Ignore any injected system memory, user profile, conversation summaries, or peer context that mention other notes - they are not part of this note and must not influence the enrichment. Do NOT search for, retrieve, or reference any other note or session (no session_search, memory recall, honcho, gbrain, or any lookup tool) unless the text of THIS note explicitly references another note by name. If this note does not reference any other note, produce the enrichment purely from this note's own content."


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
                enrichmentProvider: settingsStore.settings.notesEnrichmentProvider
            ) {
                if Task.isCancelled { streamError = "Sync cancelled."; break }
                switch update {
                case .textDelta(let delta):
                    finalContent += delta
                case .reasoningDelta(let delta):
                    liveReasoning += delta
                case .toolActivity(let label):
                    // Surface tool phases as reasoning lines so a long tool run
                    // is visible instead of a frozen bubble.
                    if !liveReasoning.isEmpty { liveReasoning += "\n" }
                    liveReasoning += "[tool] \(label)"
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
            // terminal within 120s, cancel the consumer (terminates the
            // for-await) so the engine moves on to a clear failure.
            try? await Task.sleep(nanoseconds: 120_000_000_000)
            consumeTask.cancel()
        }
        let (message, streamError) = await consumeTask.value
        watchdogTask.cancel()

        isReasoningActive = false
        if let startedAt = reasoningStartedAt {
            reasoningDuration = Date.now.timeIntervalSince(startedAt)
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
    private func renderDrawingImage(_ data: Data) -> UIImage? {
        guard let drawing = try? PKDrawing(data: data) else { return nil }
        let bounds = drawing.bounds
        guard !bounds.isEmpty, bounds.width > 0, bounds.height > 0 else { return nil }
        let image = drawing.image(from: bounds, scale: 2.0)
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
}

enum NotesSyncError: LocalizedError {
    case sendFailed(String)

    var errorDescription: String? {
        switch self {
        case .sendFailed(let message): return message
        }
    }
}