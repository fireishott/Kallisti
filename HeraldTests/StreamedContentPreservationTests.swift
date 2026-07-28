import Foundation
import Testing
@testable import Herald

@Suite("Streamed content survives an empty resolved message")
struct StreamedContentPreservationTests {

    @Test("Empty resolved content falls back to streamed text")
    func emptyResolvedKeepsStreamed() {
        let resolved = Message(sender: .herald, content: "", status: .delivered)
        let merged = ChatStore.mergeResolvedMessage(resolved: resolved, streamedContent: "Hello there")
        #expect(merged.content == "Hello there")
        #expect(merged.status == .delivered)
    }

    @Test("Non-empty resolved content wins over streamed text")
    func resolvedWinsWhenPresent() {
        let resolved = Message(sender: .herald, content: "Canonical", status: .delivered)
        let merged = ChatStore.mergeResolvedMessage(resolved: resolved, streamedContent: "partial")
        #expect(merged.content == "Canonical")
    }

    @Test("Both empty stays empty")
    func bothEmptyStaysEmpty() {
        let resolved = Message(sender: .herald, content: "", status: .delivered)
        let merged = ChatStore.mergeResolvedMessage(resolved: resolved, streamedContent: "")
        #expect(merged.content == "")
    }

    @Test("Whitespace-only resolved content is treated as empty")
    func whitespaceResolvedIsEmpty() {
        let resolved = Message(sender: .herald, content: "  \n", status: .delivered)
        let merged = ChatStore.mergeResolvedMessage(resolved: resolved, streamedContent: "Streamed")
        #expect(merged.content == "Streamed")
    }
}
