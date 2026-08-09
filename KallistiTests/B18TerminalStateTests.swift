import Foundation
import Testing
@testable import Kallisti

@MainActor
@Suite("B18 terminal state — credible completion and placeholder settlement")
struct B18TerminalStateTests {

    // MARK: - Helpers

    private func makeMsg(_ id: UUID, _ sender: MessageSender, _ content: String,
                         jobID: UUID? = nil, isStreaming: Bool = false,
                         reasoning: String = "") -> Message {
        Message(id: id, sender: sender, content: content, jobID: jobID,
                isStreaming: isStreaming, reasoning: reasoning)
    }

    private func merge(local: [Message], refreshed: [Message]) -> [Message] {
        let localConv = Conversation(id: UUID(), title: "Test", messages: local)
        let refreshedConv = Conversation(id: localConv.id, title: "Test", messages: refreshed)
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        let merged = store.mergeConversationMetadata(from: localConv, into: refreshedConv)
        return merged?.messages ?? []
    }

    // MARK: - D4: usage alone is not a reply

    @Test("Usage-only terminal completion is not credible")
    func usageOnlyTerminalIsNotCredible() {
        // Simulate what ChatStore does at .finished with empty content/reasoning
        // but non-nil usage.  The mergeResolvedMessage path handles this directly;
        // this test verifies the guard in mergeConversationMetadata doesn't flip
        // a streaming placeholder to .delivered when only usage arrived.
        //
        // We test this through the merge path: a local streaming placeholder with
        // empty content and reasoning merged into a refreshed array containing a
        // server twin with empty content/reasoning but a jobID (which is how the
        // connector delivers a usage-only terminal event).
        let job1 = UUID()
        let placeholderID = UUID()

        let local: [Message] = [
            makeMsg(UUID(), .user, "hi", jobID: job1),
            makeMsg(placeholderID, .herald, "", jobID: job1,
                    isStreaming: true, reasoning: ""),
        ]

        // Server twin: empty content, empty reasoning, but has the jobID.
        // This is the usage-only terminal case.
        let serverTwinID = UUID()
        let refreshed: [Message] = [
            makeMsg(UUID(), .user, "hi", jobID: job1),
            makeMsg(serverTwinID, .herald, "", jobID: job1, reasoning: ""),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        // The placeholder (preserved as localOnly) must NOT be streaming.
        if let placeholder = merged.first(where: { $0.id == placeholderID }) {
            #expect(!placeholder.isStreaming,
                    "Preserved streaming placeholder must have isStreaming=false")
            #expect(placeholder.status == .failed,
                    "Empty streaming placeholder should be marked failed, got \(placeholder.status)")
        }
    }

    // MARK: - D4: preserved placeholder settlement

    @Test("Preserved streaming placeholder does not stay streaming after merge")
    func preservedPlaceholderDoesNotStayStreaming() {
        let streamingID = UUID()

        let local: [Message] = [
            makeMsg(streamingID, .herald, "partial text...",
                    jobID: UUID(), isStreaming: true, reasoning: "thinking..."),
        ]

        // Refreshed has nothing — this is the POST /v1/messages 1-message-payload case
        // (now guarded by §3.4, but the full merge path still exercises this).
        let refreshed: [Message] = []

        let merged = merge(local: local, refreshed: refreshed)

        #expect(merged.count == 1)
        guard merged.count == 1 else { return }
        #expect(!merged[0].isStreaming,
                "Streaming placeholder must have isStreaming=false after merge, got true")
        // Content should be preserved — it had non-empty content.
        #expect(merged[0].content == "partial text...")
        // Status should NOT be .failed — it had content.
        #expect(merged[0].status != .failed,
                "Placeholder with content should not be marked failed")
    }
}
