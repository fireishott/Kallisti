import Foundation
import Testing
@testable import Kallisti

/// Regression tests for ChatStore draft persistence methods.
///
/// Drafts are stored in-memory on the ChatStore instance and survive
/// view recreation during reconnects.
@MainActor
@Suite("ChatStore draft persistence")
struct DraftPersistenceTests {

    private func makeStore() -> ChatStore {
        ChatStore(
            heraldClient: MockHeraldClient(),
            persistence: MockPersistence(),
            outboxBaseDirectory: FileManager.default.temporaryDirectory
                .appendingPathComponent("draft-tests-\(UUID().uuidString)", isDirectory: true)
        )
    }

    @Test("saveDraft then loadDraft returns the same text")
    func saveAndLoadDraft() {
        let store = makeStore()
        let convID = UUID()

        store.saveDraft("Hello, draft!", for: convID)
        let loaded = store.loadDraft(for: convID)

        #expect(loaded == "Hello, draft!")
    }

    @Test("loadDraft returns empty string for unknown conversation")
    func loadDraftUnknownConversation() {
        let store = makeStore()
        let loaded = store.loadDraft(for: UUID())

        #expect(loaded == "")
    }

    @Test("clearDraft removes the draft for that conversation")
    func clearDraft() {
        let store = makeStore()
        let convID = UUID()

        store.saveDraft("to be cleared", for: convID)
        store.clearDraft(for: convID)
        let loaded = store.loadDraft(for: convID)

        #expect(loaded == "")
    }

    @Test("Drafts for different conversations are isolated")
    func draftIsolation() {
        let store = makeStore()
        let alpha = UUID()
        let beta = UUID()

        store.saveDraft("Alpha's draft", for: alpha)
        store.saveDraft("Beta's draft", for: beta)

        #expect(store.loadDraft(for: alpha) == "Alpha's draft")
        #expect(store.loadDraft(for: beta) == "Beta's draft")

        store.clearDraft(for: alpha)
        #expect(store.loadDraft(for: alpha) == "")
        #expect(store.loadDraft(for: beta) == "Beta's draft")
    }

    @Test("Saving a draft overwrites the previous text for the same conversation")
    func overwriteDraft() {
        let store = makeStore()
        let convID = UUID()

        store.saveDraft("first version", for: convID)
        store.saveDraft("second version", for: convID)
        let loaded = store.loadDraft(for: convID)

        #expect(loaded == "second version")
    }

    @Test("Saving empty string stores an empty draft")
    func saveEmptyDraft() {
        let store = makeStore()
        let convID = UUID()

        store.saveDraft("something", for: convID)
        store.saveDraft("", for: convID)
        let loaded = store.loadDraft(for: convID)

        #expect(loaded == "")
    }
}
