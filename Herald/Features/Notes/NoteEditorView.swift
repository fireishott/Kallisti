import PencilKit
import PhotosUI
import SwiftUI
import VisionKit

/// Notes deliberately expose only source ink and the useful enrichment.
/// On-device OCR remains an internal assist for enrichment, never a user-facing claim.
enum NoteViewMode: String, CaseIterable, Identifiable {
    case ink
    case enriched

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .ink: "Ink"
        case .enriched: "Enriched"
        }
    }

    var systemImage: String {
        switch self {
        case .ink: "pencil.tip"
        case .enriched: "sparkles"
        }
    }
}

/// Note editor — shows the PencilKit canvas with title editing, paper styles, and attachments.
struct NoteEditorView: View {
    @Binding var noteId: UUID
    @Environment(NotesStore.self) private var notesStore
    @Environment(NotesSyncEngine.self) private var syncEngine
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(\.scenePhase) private var scenePhase
    @State private var title: String = ""
    @State private var typedText: String = ""
    @State private var isTypingActive: Bool = false
    @State private var drawing = PKDrawing()
    @State private var pageStyle: NotePageStyle = .linesMedium
    @State private var attachments: [NoteAttachment] = []
    @State private var pencilOnly: Bool = true
    @State private var viewMode: NoteViewMode = .ink

    /// Debounce timer for persisting drawings.
    @State private var persistTask: Task<Void, Never>?
    /// Debounce timer for persisting typed text.
    @State private var typedTextPersistTask: Task<Void, Never>?

    // Attachment picker state
    @State private var showPhotoPicker = false
    @State private var showDocumentScanner = false
    @State private var showAttachmentMenu = false

    // Recognition and enrichment state
    @State private var currentRecognition: NoteRecognition?
    @State private var parsedDirectives: [NoteDirective] = []
    @State private var enrichmentResult: EnrichmentResult?
    @State private var runStatus: NoteRunStatus?
    @State private var commandResults: [NoteCommandResult] = []
    @State private var showShareToNotes = false

    // Live OCR banner state - surfaces what the recognizer read from the
    // most recent stroke batch, updating as you write.
    @State private var liveOCRText: String = ""
    @State private var isOCRWorking = false
    /// Revision whose OCR was skipped because a run was in flight; the
    /// in-flight run re-reads this revision when it finishes.
    @State private var pendingOCRRRevision: Int?

    /// Serializes on-device OCR so a stroke batch doesn't queue multiple runs.

    private let recognitionCoordinator = NoteRecognitionCoordinator()

    var body: some View {
        VStack(spacing: 0) {
            // Title field
            TextField("Note Title", text: $title)
                .font(.headline)
                .textFieldStyle(.plain)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .accessibilityLabel("Note title")
                .accessibilityHint("Enter a title for this note")
                .onChange(of: title) { _, newValue in
                    updateTitle(newValue)
                }

            // Keep the surface honest: source ink or useful enrichment. OCR is internal.
            Picker("View Mode", selection: $viewMode) {
                ForEach(NoteViewMode.allCases) { mode in
                    Label(mode.displayName, systemImage: mode.systemImage)
                        .tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .accessibilityLabel("Note view mode")

            Divider()

            // Notes fix: show the thought bubble across BOTH the Ink and Enriched
            // tabs - users must see the agent is working regardless of which
            // surface they're looking at. Three states drive the bubble:
            //   1. live: a `reasoning.delta` stream is in flight.
            //   2. in-progress without deltas: sync is active OR a stage
            //      transition just happened - keep the bubble visible with
            //      honest stage text so the user is never left staring at a
            //      frozen UI.
            //   3. completed: render a collapsible "Thought for Xs" card from
            //      the last successful sync that the user can dismiss.
            // Recognized tab is gone - kept the comment intentional.
            let bubbleReasoning = effectiveBubbleReasoning
            let bubbleStreaming = syncEngine.isReasoningActive
            let bubbleDuration = syncEngine.reasoningDuration
            let showCompletedCard = !bubbleStreaming && bubbleReasoning.isEmpty
                && !syncEngine.lastCompletedReasoning.isEmpty

            if !bubbleReasoning.isEmpty || syncEngine.isReasoningActive
                || showCompletedCard {
                ReasoningView(
                    reasoning: bubbleStreaming
                        ? bubbleReasoning
                        : (showCompletedCard
                            ? syncEngine.lastCompletedReasoning
                            : bubbleReasoning),
                    isStreaming: bubbleStreaming,
                    duration: bubbleStreaming
                        ? bubbleDuration
                        : (showCompletedCard ? syncEngine.lastCompletedDuration : bubbleDuration),
                    emptyFallbackText: syncEngine.isSyncing || syncEngine.stage != .idle
                        ? syncEngine.liveStageLabel
                        : nil
                )
                .transition(.move(edge: .top).combined(with: .opacity))
                .onTapGesture(count: 2) {
                    // Double-tap the completed card to dismiss it.
                    if showCompletedCard {
                        syncEngine.dismissCompletedReasoning()
                    }
                }
            }

            // Attachment strip (Phase 3)
            if !attachments.isEmpty {
                NoteAttachmentStrip(
                    attachments: attachments,
                    onDelete: { attachment in
                        Task { await deleteAttachment(attachment) }
                    }
                )
                Divider()
            }

            // Content based on view mode — fills remaining space
            switch viewMode {
            case .ink:
                inkView
            case .enriched:
                enrichedView
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            if syncEngine.isSyncing || syncEngine.stage != .idle {
                SyncProgressBarView(
                    statusText: syncEngine.statusText,
                    currentTitle: syncEngine.currentProgress?.noteTitle,
                    index: syncEngine.currentProgress?.index,
                    total: syncEngine.currentProgress?.total,
                    failed: syncEngine.lastSyncError
                )
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        // Build 128.97: refresh the enrichment board after ANY sync completes
        // (auto-sync cadence or manual press) so the Enriched tab shows the
        // newest summary without needing to leave and reopen the note.
        .onChange(of: syncEngine.lastSyncDate) { _, _ in
            if viewMode == .enriched {
                loadRecognitionAndEnrichment()
            }
        }
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                // Pencil-only toggle
                Button {
                    pencilOnly.toggle()
                } label: {
                    Image(systemName: pencilOnly ? "pencil.tip" : "hand.draw")
                }
                .accessibilityLabel(pencilOnly ? "Pencil only mode" : "Any input mode")
                .accessibilityHint("Toggle between pencil-only and finger drawing")
                // Keyboard text toggle (Build 131.1): typed text and ink are
                // one surface - this reveals/collapses the typing field.
                Button {
                    isTypingActive.toggle()
                } label: {
                    Image(systemName: isTypingActive ? "keyboard.chevron.compact.down" : "keyboard")
                }
                .accessibilityLabel(isTypingActive ? "Hide text field" : "Show text field")
                .accessibilityHint("Toggle the keyboard text field above the canvas")
                // Sync button: push THIS note to its session (Build 128.97 -
                // never sweeps every dirty note; one note = one task)
                Button {
                    Task {
                        await syncEngine.syncNote(id: noteId)
                        loadRecognitionAndEnrichment()
                    }
                } label: {
                    if syncEngine.isSyncing {
                        ProgressView()
                    } else {
                        Image(systemName: "icloud.and.arrow.up")
                    }
                }
                .disabled(syncEngine.isSyncing)
                .accessibilityLabel("Sync note")
                .accessibilityHint("Syncs this note to its session")
                // Attachment button (Phase 3)
                Menu {
                    Button {
                        showPhotoPicker = true
                    } label: {
                        Label("Photo Library", systemImage: "photo.on.rectangle")
                    }

                    if VNDocumentCameraViewController.isSupported {
                        Button {
                            showDocumentScanner = true
                        } label: {
                            Label("Scan Document", systemImage: "doc.viewfinder")
                        }
                    }
                } label: {
                    Image(systemName: "paperclip")
                }
                .accessibilityLabel("Add attachment")

                // Paper style menu (Phase 1). On iPhone the Settings Notes
                // rows are hidden (Build 130.4), so this menu also carries
                // the paper defaults: Default lines toggle, Note width, and
                // Line height. iPad keeps the full Settings sections.
                Menu {
                    Picker("Paper Style", selection: $pageStyle) {
                        ForEach(NotePageStyle.pickerCases, id: \.self) { style in
                            Text(style.displayName).tag(style)
                        }
                    }

                    if DeviceClass.isPhone {
                        Divider()
                        Toggle("Default Lines", isOn: Binding(
                            get: { settingsStore.settings.notesDefaultLinesEnabled },
                            set: { settingsStore.settings.notesDefaultLinesEnabled = $0 }
                        ))
                        Divider()
                        // Notes fix: checkpoint history (iPhone only - iPad
                        // gets the same control via SettingsScreen). Lists
                        // the last 5 saved snapshots with a Restore action
                        // that snapshots the current state first.
                        Menu {
                            ForEach(recentCheckpoints, id: \.id) { snap in
                                Button {
                                    Task { await restoreCheckpoint(snap) }
                                } label: {
                                    Label(checkpointLabel(snap), systemImage: "clock.arrow.circlepath")
                                }
                            }
                            if recentCheckpoints.isEmpty {
                                Text("No checkpoints yet")
                            }
                            Divider()
                            Button {
                                Task { await saveCheckpointSnapshot(reason: .manual) }
                            } label: {
                                Label("Save checkpoint now", systemImage: "square.and.arrow.down")
                            }
                        } label: {
                            Label("Checkpoints", systemImage: "clock.arrow.circlepath")
                        }
                        .accessibilityLabel("Checkpoint history")
                        Divider()
                        // Line height (0 = Auto, follows paper style default)
                        LabeledContent("Line Height") {
                            Text(lineSpacingLabel)
                                .monospacedDigit()
                        }
                        Slider(
                            value: Binding(
                                get: { settingsStore.settings.notesLineSpacing },
                                set: { settingsStore.settings.notesLineSpacing = $0 }
                            ),
                            in: 12...48,
                            step: 2
                        )
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("Note options")
            }
        }
        .onChange(of: pageStyle) { _, newStyle in
            updatePageStyle(newStyle)
        }
        .onAppear {
            loadNote()
            startCheckpointLoop()
        }
        .onChange(of: noteId) { _, _ in
            persistTask?.cancel()
            persistDrawing(drawing)
            typedTextPersistTask?.cancel()
            Task { await notesStore.saveTypedText(noteId: noteId, text: typedText) }
            loadNote()
        }
        .onDisappear {
            checkpointTask?.cancel()
            checkpointTask = nil
            persistTask?.cancel()
            persistDrawing(drawing)
            typedTextPersistTask?.cancel()
            Task { await notesStore.saveTypedText(noteId: noteId, text: typedText) }
        }
        // Notes fix: flush on background. The local save must not depend on
        // the AI sync engine - iOS will background the app any moment; we
        // persist drawing + typed text + checkpoint NOW, before the OS
        // suspends us, so a force-quit or eviction never costs a stroke.
        .onChange(of: scenePhase) { _, newPhase in
            if newPhase == .background || newPhase == .inactive {
                persistTask?.cancel()
                persistDrawing(drawing)
                typedTextPersistTask?.cancel()
                let snapText = typedText
                Task {
                    await notesStore.saveTypedText(noteId: noteId, text: snapText)
                    await saveCheckpointSnapshot(reason: .backgrounding)
                }
            }
        }
        .userActivity(QuickNoteConstants.activityType) { activity in
            let displayTitle = title.isEmpty ? "Untitled Note" : title
            activity.title = displayTitle
            activity.targetContentIdentifier = QuickNoteConstants.contentIdentifier(for: noteId)
            activity.persistentIdentifier = QuickNoteConstants.contentIdentifier(for: noteId)
            activity.isEligibleForHandoff = true
            activity.isEligibleForSearch = true
        }
        .sheet(isPresented: $showPhotoPicker) {
            NotePhotoPicker { image in
                Task { await addPhotoAttachment(image) }
            }
        }
        .fullScreenCover(isPresented: $showDocumentScanner) {
            NoteDocumentScanner { images in
                Task {
                    for image in images {
                        await addScanAttachment(image)
                    }
                }
            }
        }
    }

    // MARK: - View Modes

    /// Line-height readout for the iPhone paper-options menu (Build 130.4).
    /// Mirrors SettingsScreen.lineSpacingLabel: 0 = Auto (follow paper style
    /// default), otherwise the forced spacing in points.
    private var lineSpacingLabel: String {
        let value = settingsStore.settings.notesLineSpacing
        if value <= 0 { return "Auto" }
        return "\(Int(value)) pt"
    }

    @ViewBuilder
    private var inkView: some View {
        VStack(spacing: 0) {
            // Typed text + ink are ONE surface (Build 131.1). The keyboard
            // text lives above the canvas; empty text collapses to a toggle
            // (keyboard toolbar button) so a fresh note is pure canvas until
            // you choose to type.
            if isTypingActive || !typedText.isEmpty {
                TextEditor(text: $typedText)
                    .font(.body)
                    .scrollContentBackground(.hidden)
                    .frame(minHeight: 44, maxHeight: 140)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 4)
                    .accessibilityLabel("Note text")
                    .accessibilityHint("Type the note body with the keyboard")
                    .onChange(of: typedText) { _, newValue in
                        scheduleTypedTextPersist(newValue)
                    }
                Divider()
            }

            GeometryReader { proxy in
                PencilCanvasRepresentable(
                    drawing: $drawing,
                    pageStyle: pageStyle,
                    pencilOnly: pencilOnly,
                    // The canvas always owns the available column. Resizing the
                    // writing surface during a pinch caused the viewport to jump.
                    canvasWidth: proxy.size.width,
                    // Build 130.2: user line-height override. 0 = follow the
                    // paper style default; >0 forces the actual line spacing.
                    lineSpacing: settingsStore.settings.notesLineSpacing,
                    onDrawingChanged: { newDrawing in
                        schedulePersist(newDrawing)
                    },
                    onToolUseBegan: {},
                    onToolUseEnded: {
                        // Immediate persist on pencil-up
                        persistDrawing(drawing)
                    },
                    onViewportChanged: { viewport in
                        canvasViewport = viewport
                        persistViewport(viewport)
                    },
                    initialViewport: canvasViewport
                )
            }
        }
    }

    /// Notes fix: when the live reasoning stream is empty but the engine is
    /// mid-sync, fall back to the current stage label so the bubble stays
    /// visibly alive (Preparing / Creating session / Uploading drawing /
    /// Sending). Once real `reasoning.delta` text arrives it overrides
    /// this placeholder automatically.
    private var effectiveBubbleReasoning: String {
        if !syncEngine.liveReasoning.isEmpty { return syncEngine.liveReasoning }
        if syncEngine.isReasoningActive
            || syncEngine.stage == .preparing
            || syncEngine.stage == .creatingSession
            || syncEngine.stage == .uploading
            || syncEngine.stage == .sending {
            return syncEngine.liveStageLabel
        }
        return ""
    }

    @ViewBuilder
    private var enrichedView: some View {
        if let result = enrichmentResult {
            EnrichedDocumentView(
                result: result,
                isEditingCopy: false,
                onEditCopy: { createDerivedCopy() }
            )
        } else if runStatus?.status == .queued || runStatus?.status == .claimed {
            VStack(spacing: Design.Spacing.lg) {
                ProgressView()
                    .tint(Design.Brand.accent)
                Text("Enrichment in progress...")
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.secondaryForeground)

                if !parsedDirectives.isEmpty {
                    DirectiveProgressView(
                        directives: parsedDirectives,
                        commandResults: commandResults,
                        runStatus: runStatus?.status
                    )
                    .padding(.horizontal)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView(
                "No Enrichment Yet",
                systemImage: "doc.text",
                description: Text("Tap the sync button to generate a summary board for this note.")
            )
        }
    }

    // MARK: - Loading

    private func loadNote() {
        guard let note = notesStore.notes.first(where: { $0.id == noteId }) else { return }
        title = note.title
        pageStyle = note.pageStyle
        loadViewport()
        // Reset the live OCR readout for the new note - the banner must
        // never carry text from the previous note.
        liveOCRText = ""
        isOCRWorking = false
        pendingOCRRRevision = nil
        // Reset the typing-field toggle so a fresh note starts as pure canvas.
        isTypingActive = false
        // Clear any reasoning bubble left over from the previous note. The
        // engine is a singleton, so we must explicitly wipe its stream state
        // on every editor load - otherwise the 'Thought for Xs' header from
        // the previous turn lingers over the new note.
        syncEngine.resetReasoningState()

        // Load the latest drawing revision
        Task {
            if let data = await notesStore.loadDrawing(noteId: noteId, revision: note.currentDrawingRevision) {
                if let loaded = try? PKDrawing(data: data) {
                    drawing = loaded
                }
            }
            // Load typed text body
            typedText = await notesStore.loadTypedText(noteId: noteId) ?? ""
            // Load attachments
            attachments = await notesStore.loadAttachments(noteId: noteId)
            // Load recognition and enrichment data
            loadRecognitionAndEnrichment()
        }
    }

    // MARK: - Persistence

    private func updateTitle(_ newTitle: String) {
        Task {
            if var note = notesStore.notes.first(where: { $0.id == noteId }) {
                note.title = newTitle
                note.updatedAt = .now
                await notesStore.updateNote(note)
            }
        }
    }

    private func updatePageStyle(_ newStyle: NotePageStyle) {
        Task {
            if var note = notesStore.notes.first(where: { $0.id == noteId }) {
                note.pageStyle = newStyle
                note.updatedAt = .now
                await notesStore.updateNote(note)
            }
        }
    }

    /// Notes fix: idle settle bumped from 500ms -> 2s.
    /// The `onToolUseEnded` callback already persists IMMEDIATELY when a
    /// stroke completes (`persistDrawing(drawing)` on pencil-up), so this
    /// debounce only catches the in-stroke `onDrawingChanged` ticks that
    /// fire while the finger is moving. 2s of idle without a tool-up means
    /// the user walked away mid-stroke; we settle the partial work
    /// without thrashing the disk. Pencil-up always wins.
    private func schedulePersist(_ newDrawing: PKDrawing) {
        persistTask?.cancel()
        persistTask = Task {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            persistDrawing(newDrawing)
        }
    }

    /// Notes fix: typed-text idle settle bumped to 2s for the same reason as
    /// drawing - the user pausing the keyboard for >=2s signals an
    /// intentional break worth flushing without fighting the AI checkpoint
    /// cadence (the local save path is independent of any in-flight sync).
    private func scheduleTypedTextPersist(_ newText: String) {
        typedTextPersistTask?.cancel()
        typedTextPersistTask = Task {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            await notesStore.saveTypedText(noteId: noteId, text: newText)
        }
    }

    /// Persist the drawing immediately. Called on pencil-up and on disappear.
    /// After the blob lands, kick live OCR so the banner shows what was read.
    private func persistDrawing(_ newDrawing: PKDrawing) {
        let data = newDrawing.dataRepresentation()
        guard !data.isEmpty else { return }

        Task {
            guard let note = notesStore.notes.first(where: { $0.id == noteId }) else { return }
            let newRevision = note.currentDrawingRevision + 1
            _ = await notesStore.saveDrawing(noteId: noteId, data: data, revision: newRevision)

            // Serialize OCR: if a run is in flight, record the newest
            // revision and let the active run re-read it on completion so
            // the banner always reflects the latest strokes.
            if isOCRWorking {
                if let current = pendingOCRRRevision {
                    if newRevision > current { pendingOCRRRevision = newRevision }
                } else {
                    pendingOCRRRevision = newRevision
                }
                return
            }
            await runLiveOCR(revision: newRevision, noteId: noteId)
        }
    }

    /// Run on-device recognition for the saved revision and surface the
    /// recognized text in the live readout banner. Re-reads any revision
    /// that arrived while recognition was in flight.
    private func runLiveOCR(revision: Int, noteId: UUID) async {
        var rev = revision
        while true {
            isOCRWorking = true
            let rec = await recognitionCoordinator.recognize(
                noteId: noteId,
                drawingRevision: rev
            )
            isOCRWorking = false
            if let rec, !rec.rawText.isEmpty {
                liveOCRText = rec.rawText
                currentRecognition = rec
                // Build 130.0: persist the recognition so the Recognized tab
                // and the note sync prompt keep working after reload. Before
                // this, live OCR results lived only in memory - every reopen
                // showed "No Recognition" and the sync engine never got the
                // recognized text (which also starved enrichment).
                try? await notesStore.saveRecognition(rec, noteId: noteId)
                // Re-parse directives from the live read so recognized view,
                // enrichment, and the banner all agree on the same text.
                let parser = NoteDirectiveParser()
                parsedDirectives = parser.parse(
                    text: rec.effectiveText,
                    noteId: noteId,
                    sourceTextRevision: rev.hashValue
                )
            } else if rec?.rawText.isEmpty == true {
                liveOCRText = ""
            }
            // If a newer stroke batch landed during the run, read it too.
            if let pending = pendingOCRRRevision, pending > rev {
                pendingOCRRRevision = nil
                rev = pending
                continue
            }
            break
        }
    }

    // MARK: - Recognition & Enrichment

    private func saveCorrectedText(_ corrected: String) async {
        guard var recognition = currentRecognition else { return }
        recognition.userCorrectedText = corrected
        currentRecognition = recognition

        // Re-parse directives from corrected text
        let parser = NoteDirectiveParser()
        parsedDirectives = parser.parse(
            text: corrected,
            noteId: noteId,
            sourceTextRevision: recognition.drawingRevisionId.hashValue
        )
    }

    private func createDerivedCopy() {
        guard let result = enrichmentResult else { return }
        Task {
            if let newNote = await notesStore.createNote(title: "\(result.title) (Copy)") {
                notesStore.selectedNoteId = newNote.id
            }
        }
    }

    private func loadRecognitionAndEnrichment() {
        Task {
            // Load the latest recognition from the repository
            let repo = NotesRepository()
            if let note = notesStore.notes.first(where: { $0.id == noteId }) {
                let recs = try? await repo.loadRecognitions(noteId: noteId)
                currentRecognition = recs?.last

                // Parse directives
                if let recognition = currentRecognition {
                    let parser = NoteDirectiveParser()
                    parsedDirectives = parser.parse(
                        text: recognition.effectiveText,
                        noteId: noteId,
                        sourceTextRevision: note.currentTextRevision
                    )
                }

                // Load enrichment result if available
                enrichmentResult = try? await repo.loadEnrichmentResult(noteId: noteId)
            }
        }
    }

    // MARK: - Attachments (Phase 3)

    private func addPhotoAttachment(_ image: UIImage) async {
        guard let jpegData = image.jpegData(compressionQuality: 0.85) else { return }
        let attachment = await notesStore.saveAttachment(
            noteId: noteId,
            data: jpegData,
            type: .photo,
            fileName: "photo_\(UUID().uuidString.prefix(8)).jpg",
            mimeType: "image/jpeg"
        )
        if let attachment {
            attachments.append(attachment)
        }
    }

    private func addScanAttachment(_ image: UIImage) async {
        guard let jpegData = image.jpegData(compressionQuality: 0.85) else { return }
        let attachment = await notesStore.saveAttachment(
            noteId: noteId,
            data: jpegData,
            type: .scan,
            fileName: "scan_\(UUID().uuidString.prefix(8)).jpg",
            mimeType: "image/jpeg"
        )
        if let attachment {
            attachments.append(attachment)
        }
    }

    private func deleteAttachment(_ attachment: NoteAttachment) async {
        await notesStore.deleteAttachment(attachment)
        attachments.removeAll { $0.id == attachment.id }
    }


    /// Why this checkpoint snapshot was taken. Surfaced in the menu so the
    /// user can tell an autosave from a manual save from a restore.
    enum CheckpointReason: String {
        case manual
        case backgrounding
        case automatic
        case restoreSafety = "restore-safety"
    }

    // MARK: - Checkpoint state

    /// Last few checkpoints for the current note (newest first, max 5).
    /// We pull this through a trivial getter/setter so the SwiftUI body
    /// can react to it without paying for a full @State ObservableObject.
    private var recentCheckpoints: [NoteCheckpoint] {
        get { _recentCheckpoints }
        set { _recentCheckpoints = newValue }
    }
    @State private var _recentCheckpoints: [NoteCheckpoint] = []
    @State private var lastCheckpoint: NoteCheckpoint?
    @State private var isCheckpointSheetPresented = false
    @State private var checkpointRestorationInFlight = false
    @State private var checkpointTask: Task<Void, Never>?
    @State private var canvasViewport: PencilCanvasRepresentable.CanvasViewport?

    private func startCheckpointLoop() {
        checkpointTask?.cancel()
        checkpointTask = Task {
            while !Task.isCancelled {
                guard let seconds = settingsStore.settings.notesCheckpointInterval.intervalSeconds else {
                    try? await Task.sleep(for: .seconds(30))
                    continue
                }
                try? await Task.sleep(for: .seconds(seconds))
                guard !Task.isCancelled else { return }
                await saveCheckpointSnapshot(reason: .automatic)
            }
        }
    }

    private func persistViewport(_ viewport: PencilCanvasRepresentable.CanvasViewport) {
        guard let data = try? JSONEncoder().encode(viewport) else { return }
        UserDefaults.standard.set(data, forKey: "kallisti.notes.viewport.\(noteId.uuidString)")
    }

    private func loadViewport() {
        guard let data = UserDefaults.standard.data(forKey: "kallisti.notes.viewport.\(noteId.uuidString)"),
              let viewport = try? JSONDecoder().decode(PencilCanvasRepresentable.CanvasViewport.self, from: data)
        else { return }
        canvasViewport = viewport
    }

    /// One-line label for the menu. Example: "Manual · 8:42 PM".
    private func checkpointLabel(_ cp: NoteCheckpoint) -> String {
        let time = cp.createdAt.formatted(date: .omitted, time: .shortened)
        switch cp.reason {
        case "manual": return "Manual · \(time)"
        case "backgrounding": return "Auto · \(time)"
        case "restore-safety": return "Safety · \(time)"
        default: return "Checkpoint · \(time)"
        }
    }

    /// Persist a checkpoint of the current state. Cheap - records the
    /// drawing revision + hash, doesn't copy the blob. Triggered every 5
    /// minutes during active editing, on app backgrounding, when the user
    /// taps "Save checkpoint now", and before every restore.
    private func saveCheckpointSnapshot(reason: CheckpointReason) async {
        let repo = NotesRepository()
        let safeReason: String
        switch reason {
        case .manual: safeReason = "manual"
        case .backgrounding: safeReason = "backgrounding"
        case .automatic: safeReason = "automatic"
        case .restoreSafety: safeReason = "restore-safety"
        }
        do {
            let cp = try await repo.snapshotCurrentStateAsCheckpoint(
                noteId: noteId, reason: safeReason
            )
            let recent = (try? await repo.loadCheckpoints(noteId: noteId, limit: 5)) ?? []
            await MainActor.run {
                _recentCheckpoints = recent
                lastCheckpoint = cp
            }
        } catch {
            // Best-effort; never block the user.
        }
    }

    /// RESTORE SAFETY: before applying the snapshot, take a safety
    /// checkpoint of the CURRENT state so the user can step forward again
    /// if they regret the restore. Restore is implemented as: ask the
    /// repository to atomically copy back the full checkpointed state
    /// (drawing blob, typed text, title, page style, attachments index +
    /// blob copies, enrichment result). The repository updates the
    /// KallistiNote metadata in the index to match; we then apply the
    /// returned payload to the editor's in-memory bindings so the UI
    /// reflects the restored state without an additional reload cycle.
    private func restoreCheckpoint(_ snap: NoteCheckpoint) async {
        guard !checkpointRestorationInFlight else { return }
        checkpointRestorationInFlight = true
        defer { checkpointRestorationInFlight = false }

        // Cancel pending persist tasks so they do not race the restore.
        persistTask?.cancel()
        typedTextPersistTask?.cancel()

        // Save current state FIRST - never restore without a safety snap.
        await saveCheckpointSnapshot(reason: .restoreSafety)

        let repo = NotesRepository()
        do {
            let result = try await repo.restoreCheckpoint(
                noteId: noteId, checkpointId: snap.id
            )
            await MainActor.run {
                // Title
                title = result.title
                pageStyle = result.pageStyle
                // Typed text
                typedText = result.typedText
                // Attachments metadata
                attachments = result.attachments
                // Enrichment result drives the Enriched tab
                enrichmentResult = result.enrichment
                // Drawing
                if let blob = result.drawingData,
                   let pk = try? PKDrawing(data: blob) {
                    drawing = pk
                }
            }
            // Sync the in-memory notes array from the index so the
            // restored title/drawingRevision/pageStyle are reflected
            // in the list view and any other readers.
            await notesStore.loadNotes()
            // Reload recognition (the recognitions index is shared with
            // the active state and was NOT captured by the checkpoint
            // — restore reflects whatever was true at snapshot time).
            loadRecognitionAndEnrichment()
            // Refresh the checkpoint history so the safety snapshot
            // shows up at the top.
            if let recent = try? await repo.loadCheckpoints(noteId: noteId, limit: 5) {
                await MainActor.run { _recentCheckpoints = recent }
            }
        } catch {
            // Best-effort: a failed restore leaves the user on the
            // pre-restore state. The safety snapshot is still in the
            // history so they can retry.
        }
    }

}

// MARK: - OCR Thinking Bubble

/// Chat-style thinking bubble for the live OCR readout. Shows what the
/// recognizer read as if it were a typed chat thought - complete with
/// expand/collapse chevron and 3 size/view options (compact, standard,
/// large). Matches the ReasoningView pattern used for chat reasoning.
struct NoteOCRThinkingBubble: View {
    let text: String
    let isWorking: Bool

    enum SizeOption: Int, CaseIterable, Identifiable {
        case compact, standard, large
        var id: Int { rawValue }

        var label: String {
            switch self {
            case .compact: return "Compact"
            case .standard: return "Standard"
            case .large: return "Large"
            }
        }

        var icon: String {
            switch self {
            case .compact: return "text.bubble"
            case .standard: return "rectangle"
            case .large: return "arrow.up.left.and.arrow.down.right"
            }
        }
    }

    @State private var isExpanded = false
    @State private var sizeOption: SizeOption = .standard
    @State private var startedAt: Date = .now

    private var viewportHeight: CGFloat {
        switch sizeOption {
        case .compact: return 64
        case .standard: return 132
        case .large: return max(240, UIScreen.main.bounds.height * 0.52)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            header
            if isExpanded {
                ScrollView(.vertical, showsIndicators: true) {
                    Text(text.isEmpty ? "Waiting for handwriting…" : text)
                        .font(.system(.footnote, design: .default))
                        .italic()
                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.9))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: viewportHeight)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xs)
        .background(Design.Colors.surface.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(isWorking ? Design.Brand.accent.opacity(0.35) : Design.Colors.border,
                        lineWidth: 1)
        )
        .task(id: isWorking) {
            if isWorking {
                startedAt = .now
                withAnimation(Design.Motion.standard) { isExpanded = true }
            }
        }
        .animation(Design.Motion.standard, value: sizeOption)
        .animation(Design.Motion.standard, value: isExpanded)
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Design.Spacing.xs) {
            Button {
                withAnimation(Design.Motion.standard) {
                    isExpanded.toggle()
                    if sizeOption == .large { sizeOption = .standard }
                }
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    Image(systemName: isWorking ? "brain.head.profile" : "brain")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(isWorking ? Design.Brand.accent : Design.Colors.secondaryForeground)

                    if isWorking {
                        // Chat-style live timer: "Reading handwriting… 3s"
                        TimelineView(.periodic(from: startedAt, by: 1)) { context in
                            Text("Reading handwriting… \(Int(context.date.timeIntervalSince(startedAt)))s")
                                .font(.system(.caption, weight: .medium))
                                .monospacedDigit()
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    } else {
                        Text(headerLabel)
                            .font(.system(.caption, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            Spacer(minLength: 0)

            if isExpanded {
                // 3 size/view options: compact, standard, large
                HStack(spacing: 2) {
                    ForEach(SizeOption.allCases) { option in
                        Button {
                            withAnimation(Design.Motion.standard) {
                                sizeOption = option
                                if option == .large { isExpanded = true }
                            }
                        } label: {
                            Image(systemName: option.icon)
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(sizeOption == option ? Design.Brand.accent : Design.Colors.secondaryForeground)
                                .frame(width: 28, height: 24)
                                .background(
                                    sizeOption == option ? Design.Brand.accent.opacity(0.15) : Color.clear,
                                    in: RoundedRectangle(cornerRadius: 6)
                                )
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(option.label)
                    }
                }
            }

            Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(isWorking ? Design.Brand.accent : Design.Colors.secondaryForeground)
                .frame(width: 16, height: 28)
                .contentShape(Rectangle())
                .onTapGesture {
                    withAnimation(Design.Motion.standard) {
                        isExpanded.toggle()
                        if sizeOption == .large { sizeOption = .standard }
                    }
                }
                .accessibilityLabel(isExpanded ? "Collapse OCR readout" : "Expand OCR readout")
        }
        .padding(.vertical, Design.Spacing.xxs)
    }

    private var headerLabel: String {
        if isWorking {
            return "Reading handwriting…"
        }
        return text.isEmpty ? "OCR Readout" : "Recognized"
    }
}

// MARK: - Attachment Strip

struct NoteAttachmentStrip: View {
    let attachments: [NoteAttachment]
    let onDelete: (NoteAttachment) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(attachments) { attachment in
                    AttachmentThumbnail(attachment: attachment, onDelete: {
                        onDelete(attachment)
                    })
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .frame(height: 80)
    }
}

struct AttachmentThumbnail: View {
    let attachment: NoteAttachment
    let onDelete: () -> Void
    @State private var thumbnail: UIImage?

    var body: some View {
        ZStack(alignment: .topTrailing) {
            if let thumbnail {
                Image(uiImage: thumbnail)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(width: 60, height: 60)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            } else {
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color(.systemGray5))
                    .frame(width: 60, height: 60)
                    .overlay {
                        Image(systemName: attachment.type == .scan ? "doc.viewfinder" : "photo")
                            .foregroundStyle(.secondary)
                    }
            }

            Button(action: onDelete) {
                Image(systemName: "xmark.circle.fill")
                    .font(.caption)
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(.white, .black.opacity(0.5))
            }
            .offset(x: 4, y: -4)
        }
        .task {
            loadThumbnail()
        }
    }

    private func loadThumbnail() {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: attachment.blobPath)) else { return }
        guard let image = UIImage(data: data) else { return }

        let targetSize = CGSize(width: 120, height: 120)
        let renderer = UIGraphicsImageRenderer(size: targetSize)
        thumbnail = renderer.image { _ in
            image.draw(in: CGRect(origin: .zero, size: targetSize))
        }
    }
}

// MARK: - Photo Picker (PHPickerViewController)

struct NotePhotoPicker: UIViewControllerRepresentable {
    let onPick: (UIImage) -> Void

    func makeUIViewController(context: Context) -> PHPickerViewController {
        var config = PHPickerConfiguration()
        config.filter = .images
        config.selectionLimit = 1
        let picker = PHPickerViewController(configuration: config)
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ uiViewController: PHPickerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    @MainActor
    final class Coordinator: NSObject, PHPickerViewControllerDelegate {
        let parent: NotePhotoPicker
        init(_ parent: NotePhotoPicker) { self.parent = parent }

        nonisolated func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            Task { @MainActor in
                picker.dismiss(animated: true)
            }
            guard let provider = results.first?.itemProvider,
                  provider.canLoadObject(ofClass: UIImage.self) else { return }
            provider.loadObject(ofClass: UIImage.self) { image, _ in
                if let image = image as? UIImage {
                    Task { @MainActor in
                        self.parent.onPick(image)
                    }
                }
            }
        }
    }
}

// MARK: - Document Scanner (VNDocumentCameraViewController)

struct NoteDocumentScanner: UIViewControllerRepresentable {
    let onScan: ([UIImage]) -> Void

    func makeUIViewController(context: Context) -> VNDocumentCameraViewController {
        let scanner = VNDocumentCameraViewController()
        scanner.delegate = context.coordinator
        return scanner
    }

    func updateUIViewController(_ uiViewController: VNDocumentCameraViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    @MainActor
    final class Coordinator: NSObject, VNDocumentCameraViewControllerDelegate {
        let parent: NoteDocumentScanner
        init(_ parent: NoteDocumentScanner) { self.parent = parent }

        nonisolated func documentCameraViewController(_ controller: VNDocumentCameraViewController, didFinishWith scan: VNDocumentCameraScan) {
            var images: [UIImage] = []
            for i in 0..<scan.pageCount {
                images.append(scan.imageOfPage(at: i))
            }
            Task { @MainActor in
                controller.dismiss(animated: true)
                self.parent.onScan(images)
            }
        }

        nonisolated func documentCameraViewControllerDidCancel(_ controller: VNDocumentCameraViewController) {
            Task { @MainActor in
                controller.dismiss(animated: true)
            }
        }

        nonisolated func documentCameraViewController(_ controller: VNDocumentCameraViewController, didFailWithError error: Error) {
            Task { @MainActor in
                controller.dismiss(animated: true)
            }
        }
    }
}
