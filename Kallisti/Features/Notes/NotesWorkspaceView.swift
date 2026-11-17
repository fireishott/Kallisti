import SwiftUI

/// Notes list/editor split for the Notes tab (128.78).
///
/// Was split by device class: iPad expected the app sidebar to double as the
/// notes blade. With unified bottom-tab navigation (128.77+), the Notes tab
/// owns its own NavigationSplitView on BOTH platforms so a note list is always
/// reachable on iPad.
struct NotesWorkspaceView: View {
    @Environment(NotesStore.self) private var notesStore

    var body: some View {
        @Bindable var store = notesStore

        NavigationSplitView {
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
    }
}