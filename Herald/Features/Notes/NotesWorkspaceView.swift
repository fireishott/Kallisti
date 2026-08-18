import SwiftUI

/// Notes list/editor split for the Notes tab (128.78).
///
/// Was split by device class: iPad expected the app sidebar to double as the
/// notes blade. With unified bottom-tab navigation (128.77+), the Notes tab
/// owns its own NavigationSplitView on BOTH platforms so a note list is always
/// reachable on iPad.
struct NotesWorkspaceView: View {
    @Environment(NotesStore.self) private var notesStore

    /// Build 128.99: auto-close the sidebar when a note is selected or a new
    /// one is created - the editor should get the full width immediately.
    /// Both paths funnel through `selectedNoteId` (createNote sets it), so one
    /// onChange covers select + create.
    @State private var columnVisibility: NavigationSplitViewVisibility = .all

    var body: some View {
        @Bindable var store = notesStore

        NavigationSplitView(columnVisibility: $columnVisibility) {
            NotesListView()
        } detail: {
            if let noteId = store.selectedNoteId {
                NoteEditorView(noteId: .constant(noteId))
                    .id(noteId)
            } else {
                ContentUnavailableView(
                    "No Note Selected",
                    systemImage: "pencil.and.outline",
                    description: Text("Select a note from the list or create a new one.")
                )
            }
        }
        .task {
            await notesStore.loadNotes()
        }
        .onChange(of: store.selectedNoteId) { _, newValue in
            guard newValue != nil else { return }
            withAnimation(Design.Motion.standard) {
                columnVisibility = .detailOnly
            }
        }
    }
}
