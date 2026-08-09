import SwiftUI

/// Notes-owned list/editor split in the iPad detail column.
struct NotesWorkspaceView: View {
    @Environment(NotesStore.self) private var notesStore

    var body: some View {
        @Bindable var store = notesStore

        Group {
            if DeviceClass.isPad {
                // On iPad, the sidebar IS the notes blade.
                // This view just shows the editor in the detail column.
                if let noteId = store.selectedNoteId {
                    NoteEditorView(noteId: .constant(noteId))
                        .id(noteId)
                } else {
                    ContentUnavailableView(
                        "No Note Selected",
                        systemImage: "pencil.and.outline",
                        description: Text("Select a note from the sidebar or create a new one.")
                    )
                }
            } else {
                // iPhone: keep the existing NavigationSplitView
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
            }
        }
        .task {
            await notesStore.loadNotes()
        }
    }
}
