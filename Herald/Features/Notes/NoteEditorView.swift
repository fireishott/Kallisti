import PencilKit
import SwiftUI

/// Note editor — shows the PencilKit canvas with title editing.
/// Phase 1: basic editor with PencilKit canvas.
/// Phase 2: adds recognition review.
/// Phase 3: adds enrichment view.
struct NoteEditorView: View {
    let noteId: UUID
    @Environment(NotesStore.self) private var notesStore
    @State private var title: String = ""
    @State private var drawing = PKDrawing()
    @State private var pageStyle: NotePageStyle = .letter
    @State private var showPaperBackground = true

    /// Debounce timer for persisting drawings.
    @State private var persistTask: Task<Void, Never>?

    var body: some View {
        VStack(spacing: 0) {
            // Title field
            TextField("Note Title", text: $title)
                .font(.title2)
                .textFieldStyle(.plain)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .onChange(of: title) { _, newValue in
                    updateTitle(newValue)
                }

            Divider()

            // Canvas with paper background
            ZStack {
                if showPaperBackground {
                    NotePaperBackground(style: pageStyle)
                        .allowsHitTesting(false)
                }

                PencilCanvasRepresentable(
                    drawing: $drawing,
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
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Menu {
                    Toggle("Paper Lines", isOn: $showPaperBackground)
                    Picker("Page Style", selection: $pageStyle) {
                        ForEach([NotePageStyle.letter, .a4, .blank], id: \.self) { style in
                            Text(style.rawValue.capitalized).tag(style)
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .onAppear {
            loadNote()
        }
        .onDisappear {
            persistTask?.cancel()
            persistDrawing(drawing)
        }
    }

    // MARK: - Loading

    private func loadNote() {
        guard let note = notesStore.notes.first(where: { $0.id == noteId }) else { return }
        title = note.title

        // Load the latest drawing revision
        Task {
            if let data = await notesStore.loadDrawing(noteId: noteId, revision: note.currentDrawingRevision) {
                if let loaded = try? PKDrawing(data: data) {
                    drawing = loaded
                }
            }
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
    private func persistDrawing(_ newDrawing: PKDrawing) {
        let data = newDrawing.dataRepresentation()
        guard !data.isEmpty else { return }

        Task {
            guard let note = notesStore.notes.first(where: { $0.id == noteId }) else { return }
            let newRevision = note.currentDrawingRevision + 1
            _ = await notesStore.saveDrawing(noteId: noteId, data: data, revision: newRevision)
        }
    }
}
