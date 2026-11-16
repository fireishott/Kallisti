import Foundation
import Testing
@testable import Kallisti

@MainActor
@Suite("B18 transcript ordering — local-only anchoring and identity-map resolution")
struct B18TranscriptOrderTests {

    // MARK: - Helpers

    private func makeMsg(_ id: UUID, _ sender: MessageSender, _ content: String,
                         jobID: UUID? = nil, clientMessageID: UUID? = nil,
                         isStreaming: Bool = false, reasoning: String = "") -> Message {
        Message(id: id, clientMessageID: clientMessageID, sender: sender,
                content: content, jobID: jobID, isStreaming: isStreaming,
                reasoning: reasoning)
    }

    private func merge(local: [Message], refreshed: [Message]) -> [Message] {
        let localConv = Conversation(id: UUID(), title: "Test", messages: local)
        let refreshedConv = Conversation(id: localConv.id, title: "Test", messages: refreshed)
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        let merged = store.mergeConversationMetadata(from: localConv, into: refreshedConv)
        return merged?.messages ?? []
    }

    // MARK: - Tests

    @Test("Assistant reply never precedes its own prompt")
    func assistantReplyNeverPrecedesItsPrompt() {
        let u1ID = UUID(), a1ID = UUID(), u2ID = UUID(), a2ID = UUID()
        let job2 = UUID()

        let local: [Message] = [
            makeMsg(u1ID, .user, "First question"),
            makeMsg(a1ID, .herald, "First answer", jobID: UUID()),
            makeMsg(u2ID, .user, "Second question", jobID: job2),
            makeMsg(a2ID, .herald, "", jobID: job2, isStreaming: true, reasoning: "thinking..."),
        ]

        // Full refresh (GET /conversation): the server returns u1 and a1 under
        // their original ids, but u2 under a server-assigned id with
        // clientMessageId pointing at the local u2.  a2 is absent (streaming
        // placeholder not yet persisted).  This is the exact D1 scenario.
        let serverU2ID = UUID()
        let refreshed: [Message] = [
            makeMsg(u1ID, .user, "First question"),
            makeMsg(a1ID, .herald, "First answer", jobID: UUID()),
            makeMsg(serverU2ID, .user, "Second question",
                    jobID: job2, clientMessageID: u2ID),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        // The merged array must end with a2 (the streaming reply, now settled).
        guard let lastMsg = merged.last else {
            Issue.record("merged array is empty")
            return
        }
        #expect(lastMsg.sender == .herald)
        #expect(lastMsg.id == a2ID, "Streaming placeholder should be last")

        // u2 must appear BEFORE a2.
        let u2Idx = merged.firstIndex(where: { $0.id == serverU2ID })
        let a2Idx = merged.firstIndex(where: { $0.id == a2ID })
        #expect(u2Idx != nil)
        #expect(a2Idx != nil)
        #expect(u2Idx! < a2Idx!)
    }

    @Test("History is not appended after the newest turn")
    func historyIsNotAppendedAfterTheNewestTurn() {
        let u1ID = UUID(), a1ID = UUID(), u2ID = UUID()
        let job2 = UUID()

        let local: [Message] = [
            makeMsg(u1ID, .user, "Old question"),
            makeMsg(a1ID, .herald, "Old answer", jobID: UUID()),
            makeMsg(u2ID, .user, "New question", jobID: job2),
        ]

        let serverU2ID = UUID()
        let refreshed: [Message] = [
            makeMsg(u1ID, .user, "Old question"),
            makeMsg(a1ID, .herald, "Old answer", jobID: UUID()),
            makeMsg(serverU2ID, .user, "New question",
                    jobID: job2, clientMessageID: u2ID),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        // u1 and a1 must appear at indices 0 and 1, not appended at the end.
        let u1Idx = merged.firstIndex(where: { $0.id == u1ID })
        let a1Idx = merged.firstIndex(where: { $0.id == a1ID })
        #expect(u1Idx == 0)
        #expect(a1Idx == 1)
    }

    @Test("Interleaved local-only runs keep transcript order")
    func interleavedLocalOnlyRunsKeepTranscriptOrder() {
        // Two consecutive local-only turns — neither has a server twin.
        let l1ID = UUID(), l2ID = UUID()

        let local: [Message] = [
            makeMsg(l1ID, .user, "Local-only one"),
            makeMsg(l2ID, .user, "Local-only two"),
        ]

        let refreshed: [Message] = []

        let merged = merge(local: local, refreshed: refreshed)

        #expect(merged.count == 2)
        // Must keep original relative order, not reversed.
        guard merged.count == 2 else { return }
        #expect(merged[0].id == l1ID)
        #expect(merged[1].id == l2ID)
    }

    // MARK: - WP2: D2

    @Test("Non-contiguous jobID group keeps exactly one reasoning card on the last row")
    func nonContiguousJobIDGroupKeepsOneReasoningCard() {
        let jobA = UUID()
        let serverRow1ID = UUID(), serverRow2ID = UUID()

        // Simulate: herald(job A, reasoning) → system(nil) → herald(job A, reasoning)
        // The system row breaks contiguity. Only the LAST herald row keeps reasoning.
        let refreshed: [Message] = [
            makeMsg(serverRow1ID, .herald, "Tool output",
                    jobID: jobA, reasoning: "Searching the database…"),
            Message(sender: .system, content: "--- tool boundary ---",
                    jobID: nil),  // ← nil jobId breaks contiguity
            makeMsg(serverRow2ID, .herald, "Final answer",
                    jobID: jobA, reasoning: "Cross-referencing results…"),
        ]

        let merged = merge(local: [], refreshed: refreshed)

        // Count rows with non-empty reasoning for job A.
        let reasoningRows = merged.filter { $0.jobID == jobA && !$0.reasoning.isEmpty }
        #expect(reasoningRows.count == 1,
                "Expected 1 reasoning row but got \(reasoningRows.count)")

        // It must be the LAST herald row of job A.
        if let lastIdx = merged.lastIndex(where: { $0.jobID == jobA && $0.sender == .herald }) {
            #expect(!merged[lastIdx].reasoning.isEmpty, "Last row should keep reasoning")
        }
    }
}
// Appended 2026-11-16 with the transcript fixes.
//
// The view used to re-sort the merged transcript by timestamp, mixing the phone's
// clock (optimistic rows) with the daemon's (fetched rows). A prompt stored a few
// hundred milliseconds before the phone's own timestamp sorted below the reply it
// produced, which reads as a transcript out of order. These tests pin the invariant
// that replaced it: the merged order is the order, and the live placeholder stays
// at the bottom.

@MainActor
@Suite("Transcript rows keep the merged order")
struct TranscriptRowOrderTests {

    private func makeMsg(
        _ id: UUID,
        _ sender: MessageSender,
        _ content: String,
        at timestamp: Date,
        jobID: UUID? = nil,
        isStreaming: Bool = false
    ) -> Message {
        var message = Message(id: id, clientMessageID: nil, sender: sender, content: content)
        message.timestamp = timestamp
        message.jobID = jobID
        message.isStreaming = isStreaming
        return message
    }

    /// A prompt that carries a later local timestamp than the reply it produced
    /// must still render above that reply, because the array says so.
    @Test("A clock that disagrees with the array does not reorder the transcript")
    func clocksDoNotReorder() {
        let prompt = UUID(), reply = UUID()
        let now = Date()
        let messages = [
            makeMsg(prompt, .user, "question", at: now.addingTimeInterval(5)),
            makeMsg(reply, .herald, "answer", at: now, jobID: UUID()),
        ]

        let rows = ChatScreen.transcriptRows(messages)
        #expect(rows.map(\.id) == [prompt, reply])
    }

    /// The live placeholder keeps its place at the bottom even when its creation
    /// timestamp is the oldest part of the turn.
    @Test("The streaming row is pinned last")
    func streamingRowIsPinnedLast() {
        let earlier = UUID(), live = UUID(), later = UUID()
        let now = Date()
        let messages = [
            makeMsg(earlier, .user, "question", at: now),
            makeMsg(live, .herald, "", at: now.addingTimeInterval(-30), jobID: UUID(), isStreaming: true),
            makeMsg(later, .user, "another", at: now.addingTimeInterval(10)),
        ]

        let rows = ChatScreen.transcriptRows(messages)
        #expect(rows.last?.id == live)
        #expect(rows.count == 3)
    }

    /// Nothing is dropped or duplicated on the way through.
    @Test("Every row survives exactly once")
    func rowsAreComplete() {
        let ids = (0..<6).map { _ in UUID() }
        let now = Date()
        let messages = ids.enumerated().map { index, id in
            makeMsg(id, index.isMultiple(of: 2) ? .user : .herald,
                    "row \(index)", at: now.addingTimeInterval(Double(index)))
        }

        let rows = ChatScreen.transcriptRows(messages)
        #expect(rows.map(\.id) == ids)
    }
}

// Appended 2026-11-16 with the transcript fixes.
//
// The three layers that decide whether a phone can find its own history:
//   1. the map from local chat id to daemon conversation id,
//   2. which chat is open,
//   3. binding before the turn is created.
// Each of these was missing at some point, and each produced the same symptom: an
// empty transcript. They are tested here rather than assumed.

@Suite("The conversation map survives a relaunch")
@MainActor
struct ConversationPersistenceTests {

    private func freshDefaults() {
        UserDefaults.standard.removeObject(forKey: "eris.conversationMap.v1")
        UserDefaults.standard.removeObject(forKey: "eris.openConversation.v1")
    }

    /// A map written by one client is read by the next one.
    @Test("A bound conversation is remembered")
    func mapIsRestored() {
        freshDefaults()
        let local = UUID()
        UserDefaults.standard.set(
            [local.uuidString: "conversation-abc"],
            forKey: "eris.conversationMap.v1"
        )

        let client = ErisHeraldClient()
        #expect(client.boundConversationID(for: local) == "conversation-abc")
        freshDefaults()
    }

    /// The open chat comes back, with its id and title, so the app reopens the
    /// transcript instead of minting a new chat id that the map cannot resolve.
    @Test("The open chat is reopened, not replaced")
    func openConversationIsRestored() async {
        freshDefaults()
        let id = UUID()
        UserDefaults.standard.set([id.uuidString, "Tide pools"], forKey: "eris.openConversation.v1")

        let client = ErisHeraldClient()
        let reopened = await client.loadConversation()
        #expect(reopened.id == id)
        #expect(reopened.title == "Tide pools")
        freshDefaults()
    }

    /// A first launch has nothing stored and gets a new chat, rather than a
    /// failure.
    @Test("A first launch starts a new chat")
    func firstLaunchStartsFresh() async {
        freshDefaults()
        let client = ErisHeraldClient()
        let fresh = await client.loadConversation()
        #expect(fresh.messages.isEmpty)
        #expect(fresh.title == "New Chat")
        freshDefaults()
    }
}
