import SwiftUI

/// Notes-owned list/editor split in the iPad detail column.
struct NotesWorkspaceView: View {
    @Environment(NotesStore.self) private var notesStore

    var body: some View {
        @Bindable var store = notesStore

        NavigationSplitView {
            NotesListView()
        } detail: {
            if store.selectedNoteId != nil {
                NoteEditorView(noteId: Binding(
                    get: { store.selectedNoteId! },
                    set: { store.selectedNoteId = $0 }
                ))
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
