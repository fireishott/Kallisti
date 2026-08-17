import PencilKit
import PhotosUI
import SwiftUI
import VisionKit

/// View mode for the note editor — shows ink, recognized text, or enriched document.
enum NoteViewMode: String, CaseIterable, Identifiable {
    case ink
    case recognized
    case enriched

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .ink: "Ink"
        case .recognized: "Recognized"
        case .enriched: "Enriched"
        }
    }

    var systemImage: String {
        switch self {
        case .ink: "pencil.tip"
        case .recognized: "text.viewfinder"
        case .enriched: "doc.text"
        }
    }
}

/// Note editor — shows the PencilKit canvas with title editing, paper styles, and attachments.
struct NoteEditorView: View {
    @Binding var noteId: UUID
    @Environment(NotesStore.self) private var notesStore
    @Environment(NotesSyncEngine.self) private var syncEngine
    @State private var title: String = ""
    @State private var drawing = PKDrawing()
    @State private var pageStyle: NotePageStyle = .linesMedium
    @State private var attachments: [NoteAttachment] = []
    @State private var pencilOnly: Bool = true
    @State private var viewMode: NoteViewMode = .ink

    /// Debounce timer for persisting drawings.
    @State private var persistTask: Task<Void, Never>?

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

            // View mode picker
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

            // Live OCR banner - what the recognizer read from the strokes,
            // updated on every settle. Replaces the old sync thought bubble.
            if viewMode == .ink, !liveOCRText.isEmpty || isOCRWorking {
                OCRReadoutBanner(text: liveOCRText, isWorking: isOCRWorking)
                    .transition(.move(edge: .top).combined(with: .opacity))
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
            case .recognized:
                recognizedView
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
                // Sync button: push this note to its session
                Button {
                    Task { await syncEngine.syncNow() }
                } label: {
                    if syncEngine.isSyncing {
                        ProgressView()
                    } else {
                        Image(systemName: "icloud.and.arrow.up")
                    }
                }
                .disabled(syncEngine.isSyncing)
                .accessibilityLabel("Sync note")
                .accessibilityHint("Syncs this note and all changed notes to their sessions")
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

                // Paper style menu (Phase 1)
                Menu {
                    Picker("Paper Style", selection: $pageStyle) {
                        ForEach(NotePageStyle.pickerCases, id: \.self) { style in
                            Text(style.displayName).tag(style)
                        }
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
        }
        .onChange(of: noteId) { _, _ in
            persistTask?.cancel()
            persistDrawing(drawing)
            loadNote()
        }
        .onDisappear {
            persistTask?.cancel()
            persistDrawing(drawing)
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

    @ViewBuilder
    private var inkView: some View {
        GeometryReader { proxy in
            PencilCanvasRepresentable(
                drawing: $drawing,
                pageStyle: pageStyle,
                pencilOnly: pencilOnly,
                canvasWidth: proxy.size.width,
                onDrawingChanged: { newDrawing in
                    schedulePersist(newDrawing)
                },
                onToolUseBegan: {},
                onToolUseEnded: {
                    // Immediate persist on pencil-up
                    persistDrawing(drawing)
                }
            )
        }
    }

    @ViewBuilder
    private var recognizedView: some View {
        if let recognition = currentRecognition {
            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.md) {
                    RecognizedTextReviewView(
                        recognition: recognition,
                        directives: parsedDirectives,
                        onCorrectedTextChanged: { corrected in
                            Task { await saveCorrectedText(corrected) }
                        }
                    )

                    if !parsedDirectives.isEmpty {
                        DirectiveProgressView(
                            directives: parsedDirectives,
                            commandResults: commandResults,
                            runStatus: runStatus?.status
                        )
                    }
                }
                .padding()
            }
        } else {
            ContentUnavailableView(
                "No Recognition",
                systemImage: "text.viewfinder",
                description: Text("Write with Apple Pencil and recognition will run automatically after a short delay.")
            )
        }
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
                "No Enrichment",
                systemImage: "doc.text",
                description: Text("Add a #research or #summary directive and run enrichment to generate a document.")
            )
        }
    }

    // MARK: - Loading

    private func loadNote() {
        guard let note = notesStore.notes.first(where: { $0.id == noteId }) else { return }
        title = note.title
        pageStyle = note.pageStyle
        // Reset the live OCR readout for the new note - the banner must
        // never carry text from the previous note.
        liveOCRText = ""
        isOCRWorking = false
        pendingOCRRRevision = nil

        // Load the latest drawing revision
        Task {
            if let data = await notesStore.loadDrawing(noteId: noteId, revision: note.currentDrawingRevision) {
                if let loaded = try? PKDrawing(data: data) {
                    drawing = loaded
                }
            }
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

    /// Schedule a debounced persist (300–750ms settle).
    private func schedulePersist(_ newDrawing: PKDrawing) {
        persistTask?.cancel()
        persistTask = Task {
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled else { return }
            persistDrawing(newDrawing)
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
}

// MARK: - Live OCR Readout Banner

/// Banner showing what the recognizer read from the current strokes in
/// real time - an OCR readout, not an animated status ghost. Appears under
/// the editor header while writing in ink mode and updates on every settle.
struct OCRReadoutBanner: View {
    let text: String
    let isWorking: Bool

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "text.viewfinder")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Design.Brand.accent)
                .frame(width: 18)
            if isWorking && text.isEmpty {
                Text("Reading handwriting...")
                    .font(.footnote)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else if !text.isEmpty {
                Text(text)
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(3)
                    .multilineTextAlignment(.leading)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Design.Colors.backgroundRaised)
        .overlay(
            Rectangle()
                .fill(Design.Brand.accent.opacity(0.25))
                .frame(width: 2),
            alignment: .leading
        )
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
