import SwiftUI

/// List of notes with search, sort, and management actions.
struct NotesListView: View {
    @Environment(NotesStore.self) private var notesStore
    @Environment(NotesSyncEngine.self) private var syncEngine
    @State private var searchQuery = ""
    @State private var sortOrder: NoteSortOrder = .updatedAt
    @State private var showDeleted = false
    @State private var noteToRename: KallistiNote?
    @State private var newTitle = ""

    var body: some View {
        @Bindable var store = notesStore

        List(selection: $store.selectedNoteId) {
            // Active notes section
            if !showDeleted {
                Section {
                    ForEach(filteredNotes, id: \.id) { note in
                        NoteRowView(note: note)
                            .tag(note.id)
                            .contextMenu {
                                noteContextMenu(note)
                            }
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                Button(role: .destructive) {
                                    Task { await notesStore.deleteNote(id: note.id) }
                                } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                            .swipeActions(edge: .leading) {
                                Button {
                                    Task { await notesStore.togglePin(id: note.id) }
                                } label: {
                                    Label(
                                        note.pinned ? "Unpin" : "Pin",
                                        systemImage: note.pinned ? "pin.slash" : "pin"
                                    )
                                }
                                .tint(Design.Brand.accent)
                            }
                    }
                } header: {
                    Text("\(filteredNotes.count) notes")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // Deleted notes section (when showing deleted)
            if showDeleted && !notesStore.deletedNotes.isEmpty {
                Section("Recently Deleted") {
                    ForEach(notesStore.deletedNotes, id: \.id) { note in
                        NoteRowView(note: note, showDeleted: true)
                            .tag(note.id)
                            .contextMenu {
                                Button {
                                    Task { await notesStore.restoreNote(id: note.id) }
                                } label: {
                                    Label("Restore", systemImage: "arrow.uturn.backward")
                                }
                            }
                    }
                }
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
        .navigationTitle("Notes")
        .searchable(text: $searchQuery, prompt: "Search notes")
        // Build 128.99: prominent New Note button pinned to the bottom of the
        // side blade. Uses the custom handwriting pen vector. Creates a note
        // and selects it; the workspace auto-closes the sidebar on selection.
        .safeAreaInset(edge: .bottom, spacing: 0) {
            Button {
                Task { _ = await notesStore.createNote() }
            } label: {
                HStack(spacing: 8) {
                    HandwritingPenVector(ink: Design.Colors.background)
                        .frame(width: 18, height: 18)
                    Text("New Note")
                        .font(.headline)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 12)
                .foregroundStyle(Design.Colors.background)
                .background(Design.Brand.accent)
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("New Note")
            .accessibilityHint("Create a new note and open the editor")
        }
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button {
                    Task { await syncEngine.syncNow() }
                } label: {
                    if syncEngine.isSyncing {
                        ProgressView()
                    } else {
                        Image(systemName: syncEngine.hasPendingSync ? "icloud.and.arrow.up" : "icloud.and.arrow.up")
                    }
                }
                .disabled(syncEngine.isSyncing)
                .help("Sync notes to sessions")

                Menu {
                    Button {
                        Task { _ = await notesStore.createNote() }
                    } label: {
                        Label("New Note", systemImage: "plus")
                    }

                    Divider()

                    Picker("Sort", selection: $sortOrder) {
                        ForEach(NoteSortOrder.allCases, id: \.self) { order in
                            Text(order.displayName).tag(order)
                        }
                    }

                    Divider()

                    Toggle(isOn: $showDeleted) {
                        Label("Show Deleted", systemImage: "trash")
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .alert("Rename Note", isPresented: .constant(noteToRename != nil)) {
            TextField("Title", text: $newTitle)
            Button("Rename") {
                if let note = noteToRename {
                    var updated = note
                    updated.title = newTitle
                    Task { await notesStore.updateNote(updated) }
                }
                noteToRename = nil
            }
            Button("Cancel", role: .cancel) {
                noteToRename = nil
            }
        } message: {
            Text("Enter a new title for this note.")
        }
    }

    // MARK: - Filtering & Sorting

    private var filteredNotes: [KallistiNote] {
        let notes = notesStore.activeNotes
        let filtered: [KallistiNote]
        if searchQuery.isEmpty {
            filtered = notes
        } else {
            filtered = notes.filter { note in
                note.title.localizedCaseInsensitiveContains(searchQuery)
            }
        }

        switch sortOrder {
        case .updatedAt:
            return filtered.sorted { $0.updatedAt > $1.updatedAt }
        case .createdAt:
            return filtered.sorted { $0.createdAt > $1.createdAt }
        case .title:
            return filtered.sorted { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedAscending }
        }
    }

    // MARK: - Context Menu

    @ViewBuilder
    private func noteContextMenu(_ note: KallistiNote) -> some View {
        Button {
            Task { await notesStore.togglePin(id: note.id) }
        } label: {
            Label(note.pinned ? "Unpin" : "Pin", systemImage: note.pinned ? "pin.slash" : "pin")
        }

        Button {
            newTitle = note.title
            noteToRename = note
        } label: {
            Label("Rename", systemImage: "pencil")
        }

        Divider()

        Button(role: .destructive) {
            Task { await notesStore.deleteNote(id: note.id) }
        } label: {
            Label("Delete", systemImage: "trash")
        }
    }
}

// MARK: - Note Row

struct NoteRowView: View {
    let note: KallistiNote
    var showDeleted: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                if note.pinned {
                    Image(systemName: "pin.fill")
                        .font(.caption)
                        .foregroundStyle(Design.Brand.accent)
                }
                Text(note.title.isEmpty ? "Untitled" : note.title)
                    .font(.headline)
                    .lineLimit(1)
            }

            if showDeleted, let daysLeft = note.daysUntilPurge {
                Text("\(daysLeft) days until permanent deletion")
                    .font(.caption)
                    .foregroundStyle(.red)
            } else {
                Text(note.updatedAt.formatted(.relative(presentation: .named)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(note.pinned ? "Pinned: \(note.title.isEmpty ? "Untitled" : note.title)" : note.title.isEmpty ? "Untitled" : note.title)
        .accessibilityHint(showDeleted ? "Deleted note" : "Double-tap to open note")
    }
}

// MARK: - Handwriting Pen Vector

/// Custom handwriting pen vector for the side blade's New Note button.
/// A fountain-pen nib with a trailing ink stroke, drawn as a pure SwiftUI
/// path so it renders crisp at any size with no asset catalog dependency.
struct HandwritingPenVector: View {
    /// Ink color. Defaults to the current theme's accent for a filled button;
    /// pass a different color when placed on an accent background.
    var ink: Color = Design.Brand.accent
    var trailInk: Color? = nil

    var body: some View {
        ZStack {
            // Trailing ink stroke - the "handwriting" motion line.
            InkTrailShape()
                .stroke(trailInk ?? ink.opacity(0.55), style: StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
            // Fountain pen nib silhouette.
            PenNibShape()
                .fill(ink)
                .rotationEffect(.degrees(-45))
        }
        .accessibilityHidden(true)
    }
}

/// The nib: a pointed fountain-pen tip with a breather slit, drawn pointing
/// up-right in a 24x24 box. Rotated by `HandwritingPenVector` to a writing
/// angle.
private struct PenNibShape: Shape {
    func path(in rect: CGRect) -> Path {
        let s = min(rect.width, rect.height) / 24
        func pt(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: rect.midX + (x - 12) * s, y: rect.midY + (y - 12) * s)
        }
        var path = Path()
        // Nib body: tip at upper right, flaring to the feed at lower left.
        path.move(to: pt(17.5, 4.0))
        path.addCurve(to: pt(12.0, 13.5), control1: pt(16.0, 8.5), control2: pt(14.0, 12.0))
        path.addLine(to: pt(8.5, 11.5))
        path.addCurve(to: pt(17.5, 4.0), control1: pt(10.5, 7.5), control2: pt(14.5, 4.8))
        path.closeSubpath()
        // Breather slit from the tip down into the body.
        path.move(to: pt(16.6, 5.6))
        path.addLine(to: pt(12.6, 10.2))
        return path
    }
}

/// The ink trail: a quick hand stroke arcing from the bottom-left toward the
/// nib. Stroked, not filled.
private struct InkTrailShape: Shape {
    func path(in rect: CGRect) -> Path {
        let s = min(rect.width, rect.height) / 24
        func pt(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: rect.midX + (x - 12) * s, y: rect.midY + (y - 12) * s)
        }
        var path = Path()
        path.move(to: pt(3.0, 19.5))
        path.addCurve(to: pt(9.0, 15.5), control1: pt(4.5, 17.5), control2: pt(6.5, 16.5))
        path.addCurve(to: pt(13.0, 13.0), control1: pt(10.5, 14.8), control2: pt(11.8, 13.9))
        return path
    }
}

// MARK: - Sort Order

enum NoteSortOrder: String, CaseIterable {
    case updatedAt
    case createdAt
    case title

    var displayName: String {
        switch self {
        case .updatedAt: "Last Modified"
        case .createdAt: "Created"
        case .title:     "Title"
        }
    }
}


// MARK: - Sync Progress Bar

/// Realtime sync progress indicator shown across the notes UI.
struct SyncProgressBarView: View {
    let statusText: String
    let currentTitle: String?
    let index: Int?
    let total: Int?
    let failed: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                if let failed {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                } else {
                    ProgressView()
                        .controlSize(.small)
                }
                Text(statusText)
                    .font(.footnote.weight(.medium))
                    .lineLimit(1)
                Spacer()
                if let index, let total {
                    Text("\(index)/\(total)")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            if !failed.isEmptyOrNil, let currentTitle {
                Text(currentTitle)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            ProgressView(value: progressFraction)
                .tint(failed != nil ? .red : Design.Brand.accent)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .padding(.horizontal, 10)
        .padding(.top, 4)
    }

    private var progressFraction: Double {
        guard let index, let total, total > 0 else { return 0 }
        return Double(index) / Double(total)
    }
}

private extension Optional where Wrapped == String {
    var isEmptyOrNil: Bool { self?.isEmpty ?? true }
}
