import XCTest
@testable import Herald

@MainActor
final class QuickNotesTests: XCTestCase {
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

    // MARK: - Quick Note Activity Constants

    func test_quickNoteActivityType_matchesBundlePrefix() {
        XCTAssertEqual(
            QuickNoteConstants.activityType,
            "net.fihonline.herald.viewNote"
        )
    }

    func test_quickNoteActivityPrefix_correct() {
        XCTAssertEqual(QuickNoteConstants.contentIdentifierPrefix, "note-")
    }

    // MARK: - Create Note from Shared Text

    func test_createNoteFromSharedText_createsNoteWithProvidedTitle() async {
        await mockRepository.setNotes([])

        let note = await sut.createNoteFromSharedText(
            "Hello from another app",
            title: "Shared Note"
        )

        XCTAssertNotNil(note)
        XCTAssertEqual(note?.title, "Shared Note")
    }

    func test_createNoteFromSharedText_usesDefaultTitleWhenNil() async {
        await mockRepository.setNotes([])

        let note = await sut.createNoteFromSharedText("Some text", title: nil)

        XCTAssertNotNil(note)
        XCTAssertEqual(note?.title, "Shared Note")
    }

    func test_createNoteFromSharedText_selectsNewNote() async {
        await mockRepository.setNotes([])

        let note = await sut.createNoteFromSharedText("Test content", title: "Test")

        XCTAssertEqual(sut.selectedNoteId, note?.id)
    }

    func test_createNoteFromSharedText_returnsNilForEmptyText() async {
        await mockRepository.setNotes([])

        let note = await sut.createNoteFromSharedText("", title: "Test")

        XCTAssertNil(note)
    }

    // MARK: - URL Parsing for Share Scheme

    func test_parseShareURL_extractsText() {
        let text = "Hello World"
        let encoded = text.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let url = URL(string: "herald://share?text=\(encoded)")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNotNil(parsed)
        XCTAssertEqual(parsed?.text, "Hello World")
    }

    func test_parseShareURL_extractsTitle() {
        let url = URL(string: "herald://share?text=hello&title=My%20Note")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNotNil(parsed)
        XCTAssertEqual(parsed?.title, "My Note")
    }

    func test_parseShareURL_titleDefaultsToNil() {
        let url = URL(string: "herald://share?text=hello")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNotNil(parsed)
        XCTAssertNil(parsed?.title)
    }

    func test_parseShareURL_returnsNilForMissingText() {
        let url = URL(string: "herald://share?title=Test")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNil(parsed)
    }

    func test_parseShareURL_returnsNilForWrongScheme() {
        let url = URL(string: "other://share?text=hello")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNil(parsed)
    }

    func test_parseShareURL_returnsNilForWrongHost() {
        let url = URL(string: "herald://chat?text=hello")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNil(parsed)
    }

    func test_parseShareURL_handlesMultilineText() {
        let text = "line1%0Aline2%0Aline3"
        let url = URL(string: "herald://share?text=\(text)")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNotNil(parsed)
        XCTAssertEqual(parsed?.text, "line1\nline2\nline3")
    }

    func test_parseShareURL_handlesSpecialCharacters() {
        let text = "hello%20world%21%40%23"
        let url = URL(string: "herald://share?text=\(text)")!

        let parsed = ShareURLParser.parse(url)

        XCTAssertNotNil(parsed)
        XCTAssertEqual(parsed?.text, "hello world!@#")
    }
}
