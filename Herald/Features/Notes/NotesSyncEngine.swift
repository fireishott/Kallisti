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
                return "Sending \"\\(currentProgress.noteTitle)\" (\\(currentProgress.index)/\\(currentProgress.total))..."
            }
            return "Sending..."
        case .done:
            return "Notes synced"
        case .failed(let message):
            return "Sync failed: \\(message)"
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

    /// Sync every note that changed since its last sync. Respects the
    /// configured interval when called from the timer.
    func syncIfNeeded(trigger: SyncTrigger = .automatic) async {
        guard !isSyncing else { return }
        isSyncing = true
        stage = .preparing
        lastSyncError = nil

        // Gather dirty notes: changed drawing since last sync, or never synced.
        let notes = notesStore.activeNotes
        let dirty = notes.filter { note in
            note.currentDrawingRevision > note.lastSyncedDrawingRevision
        }
        pendingNotesCount = dirty.count
        if dirty.isEmpty {
            stage = .idle
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
                logger.error("Sync failed for note \\(note.id): \\(error.localizedDescription)")
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
                if let attachment = PendingAttachment.image(image, fileName: "note-\\(note.id.uuidString.prefix(8)).jpg") {
                    attachments.append(attachment)
                }
            }
        }

        // Build the session title from the note's current title.
        let sessionTitle = note.title.isEmpty ? "Untitled Note" : note.title

        // Text is optional for drawings; include the title as context and
        // attachment count so the agent knows what arrived.
        var messageText = "Note sync: \\(sessionTitle)"
        if !attachments.isEmpty {
            messageText += " (handwritten drawing attached)"
        } else {
            messageText += " - note has no drawable content."
        }

        // This call creates the session on first sync (titled with the note
        // name) and appends a new message to it on every later edit. The
        // conversationID is stable per note, so the session thread persists.
        stage = .creatingSession
        let message = await client.sendNoteMessage(
            text: messageText,
            attachments: attachments,
            clientMessageID: UUID(),
            conversationID: note.id,
            title: sessionTitle
        )

        if message.status == .failed || message.content.hasPrefix("Note sync error") || message.content.hasPrefix("Note sync requires") {
            throw NotesSyncError.sendFailed(message.content)
        }

        // Persist the sync checkpoint on the note so we don't re-send the
        // same revision next cycle.
        if var stored = notesStore.notes.first(where: { $0.id == note.id }) {
            stored.lastSyncedDrawingRevision = note.currentDrawingRevision
            stored.lastSyncedAt = .now
            stored.syncState = .synced
            await notesStore.updateNote(stored)
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
}

enum NotesSyncError: LocalizedError {
    case sendFailed(String)

    var errorDescription: String? {
        switch self {
        case .sendFailed(let message): return message
        }
    }
}