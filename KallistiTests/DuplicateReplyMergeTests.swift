import Foundation
import Testing
@testable import Kallisti

/// 2026-08-07 — every assistant reply rendered as two identical bubbles on
/// build 0.1.0 (130), immediately after the SSE wire-format fix made the
/// local (SSE-driven) side of `mergeConversationMetadata` carry real content
/// for the first time (previously it was always empty, so whichever bug
/// this guards against was invisible — the empty local copy never survived
/// to collide with anything).
///
/// Confirmed server-side and connector-side clean (one canonical ledger row,
/// one persisted job_events terminal, exactly one SSE `events` connection
/// per job) — this is purely a client-side merge issue. Despite extensive
/// tracing of the existing id/clientMessageID/jobID/fingerprint matching
/// logic (all of which looks individually correct, and all 7 call sites
/// are already generation-guarded against stale pre-await snapshots), no
/// single provably-wrong line was found. Given this function has already
/// been patched for this exact failure mode about a dozen times across the
/// app's history, this guards the invariant directly instead: no two
/// assistant messages sharing a jobID may survive a merge.
@MainActor
@Suite("Duplicate assistant replies collapse on merge")
struct DuplicateReplyMergeTests {

    private func makeMsg(_ id: UUID, _ sender: MessageSender, _ content: String,
                         jobID: UUID? = nil) -> Message {
        Message(id: id, sender: sender, content: content, jobID: jobID)
    }

    private func merge(local: [Message], refreshed: [Message]) -> [Message] {
        let localConv = Conversation(id: UUID(), title: "Test", messages: local)
        let refreshedConv = Conversation(id: localConv.id, title: "Test", messages: refreshed)
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        return store.mergeConversationMetadata(from: localConv, into: refreshedConv)?.messages ?? []
    }

    @Test("Two assistant rows sharing a jobID collapse to one")
    func duplicateJobIDCollapses() {
        let job = UUID()
        let userMsg = UUID(), replyA = UUID(), replyB = UUID()

        // Simulates the observed shape: two independently-constructed
        // representations of the same reply (different ids -- one may carry
        // the server's canonical id, the other a client-side one) both
        // present in what's about to become the displayed conversation.
        let refreshed: [Message] = [
            makeMsg(userMsg, .user, "What's cracking big homie"),
            makeMsg(replyA, .herald, "Sup. Comedy that actually hits, no filler:", jobID: job),
            makeMsg(replyB, .herald, "Sup. Comedy that actually hits, no filler:", jobID: job),
        ]

        let merged = merge(local: [], refreshed: refreshed)

        let heraldReplies = merged.filter { $0.sender == .herald && $0.jobID == job }
        #expect(heraldReplies.count == 1, "expected exactly one reply for jobID, got \(heraldReplies.count): \(merged.map(\.content))")
    }

    @Test("When duplicates differ in completeness, the richer copy survives")
    func richerDuplicateSurvives() {
        let job = UUID()
        let userMsg = UUID(), replyShort = UUID(), replyFull = UUID()

        let refreshed: [Message] = [
            makeMsg(userMsg, .user, "Recommend a movie"),
            makeMsg(replyShort, .herald, "Sup.", jobID: job),
            makeMsg(replyFull, .herald, "Sup. Comedy that actually hits, no filler: Superbad, The Other Guys...", jobID: job),
        ]

        let merged = merge(local: [], refreshed: refreshed)

        let heraldReplies = merged.filter { $0.sender == .herald && $0.jobID == job }
        #expect(heraldReplies.count == 1)
        #expect(heraldReplies.first?.id == replyFull, "the shorter duplicate should have been dropped, not the fuller one")
    }

    @Test("Messages from different jobs are never collapsed together")
    func differentJobsUnaffected() {
        let job1 = UUID(), job2 = UUID()
        let reply1 = UUID(), reply2 = UUID()

        let refreshed: [Message] = [
            makeMsg(reply1, .herald, "First answer", jobID: job1),
            makeMsg(reply2, .herald, "Second answer", jobID: job2),
        ]

        let merged = merge(local: [], refreshed: refreshed)

        #expect(merged.count == 2, "unrelated replies from different jobs must both survive")
    }

    // MARK: - A live turn survives a transcript refresh

    /// The live row is the one every arriving frame names. A merge that adopts
    /// the daemon's row for the same job retargets the stream at a row that is
    /// no longer in the conversation: the bubble freezes on its last painted
    /// text while the turn runs on, and only a reload shows progress. That is
    /// the "I have to leave the chat and come back" report (2026-11-06).
    @Test("A live streaming placeholder survives a refresh and keeps its id")
    func livePlaceholderSurvivesRefresh() {
        let job = UUID()
        let userId = UUID()
        let placeholder = Message(
            id: UUID(), sender: .herald,
            content: "Morning, Curtis.",
            jobID: job, isStreaming: true
        )
        let serverRow = Message(
            id: UUID(), sender: .herald,
            content: "Morning, Curtis. Let me get into the warehouse and see where the tinfoil indicator stands.",
            jobID: job, isStreaming: true
        )
        let localConversation = Conversation(
            id: UUID(), title: "Test",
            messages: [
                Message(id: userId, sender: .user, content: "Good morning", status: .sending),
                placeholder,
            ]
        )
        let refreshedConversation = Conversation(
            id: localConversation.id, title: "Test",
            messages: [
                Message(id: userId, sender: .user, content: "Good morning", status: .sent),
                serverRow,
            ]
        )
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        store.markLiveStreamForTesting(placeholderID: placeholder.id, jobID: job)
        let merged = store.mergeConversationMetadata(
            from: localConversation,
            into: refreshedConversation
        )?.messages ?? []

        let replies = merged.filter { $0.sender == .herald && $0.jobID == job }
        #expect(replies.count == 1, "one projection per turn, got \(replies.count)")
        #expect(replies.first?.id == placeholder.id,
                "the live row's id is what the stream writes through")
        #expect(replies.first?.content.contains("tinfoil indicator") == true,
                "the server's newer text is folded into the live row")
        #expect(replies.first?.isStreaming == true)
    }

    @Test("Without a live consumer the server row is authoritative")
    func serverRowWinsWithNoLiveConsumer() {
        let job = UUID()
        let userId = UUID()
        let placeholder = Message(
            id: UUID(), sender: .herald,
            content: "Morning, Curtis.",
            jobID: job, isStreaming: true
        )
        let serverRow = Message(
            id: UUID(), sender: .herald,
            content: "Morning, Curtis. Let me get into the warehouse and see where the tinfoil indicator stands.",
            jobID: job, isStreaming: true
        )
        let localConversation = Conversation(
            id: UUID(), title: "Test",
            messages: [
                Message(id: userId, sender: .user, content: "Good morning", status: .sending),
                placeholder,
            ]
        )
        let refreshedConversation = Conversation(
            id: localConversation.id, title: "Test",
            messages: [
                Message(id: userId, sender: .user, content: "Good morning", status: .sent),
                serverRow,
            ]
        )
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        let merged = store.mergeConversationMetadata(
            from: localConversation,
            into: refreshedConversation
        )?.messages ?? []

        let replies = merged.filter { $0.sender == .herald && $0.jobID == job }
        #expect(replies.count == 1)
        #expect(replies.first?.id == serverRow.id,
                "with nothing reading the stream the daemon's row is the transcript")
    }
}
