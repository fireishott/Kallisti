import XCTest
@testable import Herald

@MainActor
final class NotesStoreTests: XCTestCase {
    var sut: NotesStore!
    var mockRepository: MockNotesRepository!
    
    override func setUp() {
        super.setUp()
        mockRepository = MockNotesRepository()
        sut = NotesStore(repository: mockRepository)
    }
    
    override func tearDown() {
        sut = nil
        mockRepository = nil
        super.tearDown()
    }
    
    // MARK: - Recent Notes Tests
    
    func test_recentNotes_returnsLast10NotesSortedByUpdatedAt() async {
        // Given: 15 notes with different updatedAt dates
        let notes = (0..<15).map { i in
            HeraldNote(
                id: UUID(),
                title: "Note \(i)",
                updatedAt: Date().addingTimeInterval(Double(i) * 60)
            )
        }
        await mockRepository.setNotes(notes)
        
        // When: Getting recent notes
        await sut.loadNotes()
        let recentNotes = sut.recentNotes
        
        // Then: Should return 10 notes sorted by updatedAt descending
        XCTAssertEqual(recentNotes.count, 10)
        XCTAssertEqual(recentNotes.first?.title, "Note 14")
        XCTAssertEqual(recentNotes.last?.title, "Note 5")
    }
    
    func test_recentNotes_returnsAllNotesWhenLessThan10() async {
        // Given: 5 notes
        let notes = (0..<5).map { i in
            HeraldNote(id: UUID(), title: "Note \(i)")
        }
        await mockRepository.setNotes(notes)
        
        // When: Getting recent notes
        await sut.loadNotes()
        let recentNotes = sut.recentNotes
        
        // Then: Should return all 5 notes
        XCTAssertEqual(recentNotes.count, 5)
    }
    
    func test_recentNotes_excludesDeletedNotes() async {
        // Given: 3 active notes and 2 deleted notes
        let activeNotes = (0..<3).map { i in
            HeraldNote(id: UUID(), title: "Active \(i)")
        }
        let deletedNotes = (0..<2).map { i in
            HeraldNote(id: UUID(), title: "Deleted \(i)", deletedAt: .now)
        }
        await mockRepository.setNotes(activeNotes + deletedNotes)
        
        // When: Getting recent notes
        await sut.loadNotes()
        let recentNotes = sut.recentNotes
        
        // Then: Should only return active notes
        XCTAssertEqual(recentNotes.count, 3)
        XCTAssertTrue(recentNotes.allSatisfy { $0.title.hasPrefix("Active") })
    }
    
    // MARK: - Quick Note Tests
    
    func test_createQuickNote_createsNoteWithTitle() async {
        // Given: Empty notes store
        await mockRepository.setNotes([])
        
        // When: Creating a quick note
        let note = await sut.createQuickNote()
        
        // Then: Should create note with "Quick Note" title
        XCTAssertNotNil(note)
        XCTAssertEqual(note?.title, "Quick Note")
        XCTAssertTrue(note?.pinned ?? false)
    }
    
    func test_createQuickNote_selectsNewNote() async {
        // Given: Empty notes store
        await mockRepository.setNotes([])
        
        // When: Creating a quick note
        let note = await sut.createQuickNote()
        
        // Then: Should select the new note
        XCTAssertEqual(sut.selectedNoteId, note?.id)
    }
}
