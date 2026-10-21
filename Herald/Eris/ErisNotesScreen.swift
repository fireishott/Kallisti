import SwiftUI

// MARK: - Notes model

/// The device's notes over the `/v1/notes` routes.
@MainActor
@Observable
final class ErisNotesModel {
    private(set) var notes: [ErisNote] = []
    private(set) var isLoading = false
    private(set) var hasLoaded = false
    private(set) var failure: String?

    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    func load() async {
        isLoading = true
        failure = nil
        defer { isLoading = false; hasLoaded = true }
        do {
            notes = try await client.notes().data
        } catch {
            notes = []
            failure = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    @discardableResult
    func create(title: String) async -> ErisNote? {
        failure = nil
        do {
            let note = try await client.createNote(ErisNoteCreatePayload(title: title)).data
            await load()
            return note
        } catch {
            failure = (error as? ErisError)?.errorDescription ?? error.localizedDescription
            return nil
        }
    }

    @discardableResult
    func update(_ note: ErisNote, title: String, pinned: Bool) async -> ErisNote? {
        failure = nil
        do {
            let updated = try await client.updateNote(
                id: note.id,
                revision: note.revision,
                payload: ErisNotePatchPayload(title: title, folderId: nil, pinned: pinned)
            ).data
            if let index = notes.firstIndex(where: { $0.id == note.id }) {
                notes[index] = updated
            }
            return updated
        } catch {
            failure = (error as? ErisError)?.errorDescription ?? error.localizedDescription
            return nil
        }
    }

    func delete(_ note: ErisNote) async {
        failure = nil
        do {
            try await client.deleteNote(id: note.id)
            await load()
        } catch {
            failure = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    func clearFailure() {
        failure = nil
    }
}

// MARK: - Notes screen

/// The notes list: create, open/edit (If-Match gated), delete.
struct ErisNotesScreen: View {
    let model: ErisNotesModel

    @State private var editorNote: ErisNote?
    @State private var isCreating = false
    @State private var draftTitle = ""

    var body: some View {
        Group {
            if model.isLoading, !model.hasLoaded {
                ErisLoadingView(label: "Reading your notes…")
            } else if let failure = model.failure, model.notes.isEmpty {
                ErisFailureView(
                    kind: .other(message: failure, code: nil),
                    onRetry: { Task { await model.load() } },
                    onPairAgain: nil
                )
                .padding(Design.Spacing.xl)
            } else if model.notes.isEmpty {
                emptyView
            } else {
                noteList
            }
        }
        .background(Design.Colors.background)
        .navigationTitle("Notes")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.loadIfNeeded() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    draftTitle = ""
                    isCreating = true
                } label: {
                    Label("New Note", systemImage: "square.and.pencil")
                        .font(Design.Typography.footnote)
                }
                .accessibilityIdentifier("eris.newNote")
            }
        }
        .sheet(isPresented: $isCreating) {
            ErisNoteEditorView(
                title: "New Note",
                initialTitle: draftTitle,
                initialPinned: false,
                isNew: true,
                onSave: { title, pinned in
                    await model.create(title: title)
                    return model.failure == nil
                }
            )
        }
        .sheet(item: $editorNote) { note in
            ErisNoteEditorView(
                title: "Edit Note",
                initialTitle: note.title,
                initialPinned: note.pinned,
                isNew: false,
                onSave: { title, pinned in
                    await model.update(note, title: title, pinned: pinned)
                    return model.failure == nil
                }
            )
        }
        .alert("Error", isPresented: Binding(
            get: { model.failure != nil },
            set: { if !$0 { model.clearFailure() } }
        )) {
            Button("OK", role: .cancel) { model.clearFailure() }
        } message: {
            Text(model.failure ?? "")
        }
    }

    private var noteList: some View {
        List {
            ForEach(model.notes) { note in
                Button {
                    editorNote = note
                } label: {
                    noteRow(note)
                }
                .buttonStyle(.plain)
                .swipeActions {
                    Button(role: .destructive) {
                        Task { await model.delete(note) }
                    } label: {
                        Label("Delete", systemImage: "trash")
                    }
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
    }

    private func noteRow(_ note: ErisNote) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
                Text(note.title.isEmpty ? "Untitled" : note.title)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                Text("rev \(note.revision)")
                    .font(Design.Typography.helper)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
            Spacer()
            if note.pinned {
                Image(systemName: "pin.fill")
                    .foregroundStyle(Design.Brand.accent)
            }
        }
        .padding(.vertical, Design.Spacing.xs)
    }

    private var emptyView: some View {
        VStack(spacing: Design.Spacing.md) {
            Image(systemName: "pencil.and.outline")
                .font(.system(size: 40))
                .foregroundStyle(Design.Colors.tertiaryForeground)
            Text("No notes yet")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            Text("Tap the pencil to write the first one.")
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .padding(Design.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

// MARK: - Note editor

/// Create or edit a note's metadata (title + pinned). Edit is gated by the
/// note's revision through the If-Match header.
struct ErisNoteEditorView: View {
    let title: String
    let initialTitle: String
    let initialPinned: Bool
    let isNew: Bool
    let onSave: @MainActor (String, Bool) async -> Bool

    @Environment(\.dismiss) private var dismiss
    @State private var titleText: String
    @State private var pinned: Bool
    @State private var isSaving = false

    init(
        title: String,
        initialTitle: String,
        initialPinned: Bool,
        isNew: Bool,
        onSave: @escaping @MainActor (String, Bool) async -> Bool
    ) {
        self.title = title
        self.initialTitle = initialTitle
        self.initialPinned = initialPinned
        self.isNew = isNew
        self.onSave = onSave
        _titleText = State(initialValue: initialTitle)
        _pinned = State(initialValue: initialPinned)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Title") {
                    TextField("Note title", text: $titleText)
                }
                Section {
                    Toggle("Pinned", isOn: $pinned)
                }
                if !isNew {
                    Section {
                        Text("Saving uses the note's revision, so a stale edit is refused rather than overwriting a newer change.")
                            .font(Design.Typography.helper)
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Design.Colors.background)
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .font(Design.Typography.footnote)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        isSaving = true
                        Task {
                            let saved = await onSave(titleText.trimmingCharacters(in: .whitespacesAndNewlines), pinned)
                            isSaving = false
                            if saved { dismiss() }
                        }
                    }
                    .font(Design.Typography.footnote)
                    .disabled(isSaving || titleText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
    }
}
