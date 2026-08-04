import Foundation
import Testing
@testable import Herald

/// B21 — the follow-up prompt renders *below* the reply it produced.
///
/// Reported on 2.4.1 (20) from the 2026-07-30 23:55 transcript.  The chat showed:
///
///     user    "Sup big homie.  Mini me need to charge his phone before bed?"  23:55
///     herald  "Mini Me's at 66% …"                             7 tools        23:57
///     herald  "Yep - it's plugged in …"                       12 tools        00:00
///     user    "Is it charging now?"                                           23:58   ← last
///
/// The backend was clean: one Hermes session
/// (`run_d67a4e48f4f342ec8380f9076f004fb8`), four rows, correct order,
/// and `GET /v1/sessions/{id}/conversation` served them in that order.  The
/// transcript was scrambled by `mergeConversationMetadata`'s anchor splice.
///
/// **Why these tests use ids the way they do.** `B18TranscriptOrderTests` gives
/// the refreshed payload the *same* UUIDs as the local messages and sets
/// `clientMessageID` on the server's user row.  The connector never emits that
/// shape.  `session_store._message_to_dict` hardcodes `"clientMessageId": None`
/// on every row, and `jobId` comes from `get_message_job_id`, which is only
/// populated for **assistant** rows and only for `_MESSAGE_JOB_MAP_TTL` = 120 s
/// after the producing job completes.  So:
///
///   * a **user** row can never be matched across the local/server id boundary;
///   * an **assistant** row can, but only inside that 120 s window.
///
/// That asymmetry is the bug.  The matching loop records the assistant reply in
/// `localToRefreshedIndex`, so it becomes anchorable; the user prompt sitting
/// between it and the streaming placeholder does not.  The walk-back therefore
/// skips the prompt, anchors the placeholder to the *previous* reply, and
/// splices it in above the prompt.
@MainActor
@Suite("B21 follow-up ordering — real connector payload shape")
struct B21FollowUpOrderTests {

    // MARK: - Helpers

    private func makeMsg(_ id: UUID, _ sender: MessageSender, _ content: String,
                         jobID: UUID? = nil, clientMessageID: UUID? = nil,
                         isStreaming: Bool = false, reasoning: String = "",
                         toolActivities: [ToolActivity] = []) -> Message {
        Message(id: id, clientMessageID: clientMessageID, sender: sender,
                content: content, jobID: jobID, toolActivities: toolActivities,
                isStreaming: isStreaming, reasoning: reasoning)
    }

    private func merge(local: [Message], refreshed: [Message]) -> [Message] {
        let localConv = Conversation(id: UUID(), title: "Test", messages: local)
        let refreshedConv = Conversation(id: localConv.id, title: "Test", messages: refreshed)
        let store = ChatStore(heraldClient: MockKallistiClient(), persistence: MockPersistence())
        return store.mergeConversationMetadata(from: localConv, into: refreshedConv)?.messages ?? []
    }

    private func order(_ messages: [Message]) -> [String] {
        messages.map { "\($0.sender):\($0.content.isEmpty ? "<streaming>" : $0.content)" }
    }

    // MARK: - The reported defect

    @Test("Streaming placeholder is spliced below the prompt, not above it")
    func placeholderStaysBelowItsPrompt() {
        // Local ids.  user1 already carries the *server's* id: the poll during
        // turn 1 replaced the optimistic copy with the server row, which is
        // what makes it anchorable and the follow-up prompt not.
        let serverU1 = UUID(), localA1 = UUID(), localU2 = UUID(), placeholder = UUID()
        let serverA1 = UUID(), serverU2 = UUID()
        let job1 = UUID(), job2 = UUID()

        let local: [Message] = [
            makeMsg(serverU1, .user, "Sup big homie.  Mini me need to charge his phone before bed?"),
            makeMsg(localA1, .herald, "Mini Me's at 66% as of a minute ago.",
                    jobID: job1, reasoning: "Checking Find My…",
                    toolActivities: [ToolActivity(label: "findmy-family")]),
            makeMsg(localU2, .user, "Is it charging now?", jobID: job2),
            makeMsg(placeholder, .herald, "", jobID: job2, isStreaming: true),
        ]

        // GET /v1/sessions/{id}/conversation, 2 s after the follow-up POST:
        // every row has a server-assigned id and clientMessageId == nil.
        // Only the assistant row carries a jobId, and only because turn 1
        // finished 34 s ago — inside the 120 s job-map TTL.
        let refreshed: [Message] = [
            makeMsg(serverU1, .user, "Sup big homie.  Mini me need to charge his phone before bed?"),
            makeMsg(serverA1, .herald, "Mini Me's at 66% as of a minute ago.", jobID: job1),
            makeMsg(serverU2, .user, "Is it charging now?"),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        let promptIdx = merged.firstIndex { $0.sender == .user && $0.content == "Is it charging now?" }
        let placeholderIdx = merged.firstIndex { $0.id == placeholder }

        #expect(promptIdx != nil, "the follow-up prompt must survive the merge")
        #expect(placeholderIdx != nil, "the streaming placeholder must survive the merge")
        if let promptIdx, let placeholderIdx {
            #expect(promptIdx < placeholderIdx,
                    "reply spliced above its own prompt — got \(order(merged))")
        }
    }

    @Test("Follow-up prompt is the last user message in the merged transcript")
    func followUpPromptStaysLast() {
        let serverU1 = UUID(), localA1 = UUID(), localU2 = UUID(), localA2 = UUID()
        let serverA1 = UUID(), serverU2 = UUID()
        let job1 = UUID(), job2 = UUID()

        // The settled state: turn 2 has finished and `.finished` replaced the
        // placeholder in place, so the reply inherits the placeholder's slot.
        let local: [Message] = [
            makeMsg(serverU1, .user, "First question"),
            makeMsg(localA1, .herald, "First answer", jobID: job1),
            makeMsg(localU2, .user, "Second question", jobID: job2),
            makeMsg(localA2, .herald, "Second answer", jobID: job2),
        ]

        let refreshed: [Message] = [
            makeMsg(serverU1, .user, "First question"),
            makeMsg(serverA1, .herald, "First answer", jobID: job1),
            makeMsg(serverU2, .user, "Second question"),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        let secondPrompt = merged.firstIndex { $0.sender == .user && $0.content == "Second question" }
        let secondAnswer = merged.firstIndex { $0.content == "Second answer" }

        #expect(secondPrompt != nil)
        #expect(secondAnswer != nil)
        if let secondPrompt, let secondAnswer {
            #expect(secondPrompt < secondAnswer,
                    "answer precedes its prompt — got \(order(merged))")
        }
    }

    @Test("Turn 1 artifacts survive the refresh that reorders turn 2")
    func turn1ArtifactsSurvive() {
        // Guards the reason the assistant row is matchable at all: the jobId
        // match is what carries toolActivities onto the server's copy.  If a
        // fix makes the user row anchorable by dropping that match, the
        // "7 tools used" chip disappears instead.
        let serverU1 = UUID(), localA1 = UUID(), localU2 = UUID(), placeholder = UUID()
        let serverA1 = UUID(), serverU2 = UUID()
        let job1 = UUID(), job2 = UUID()

        let local: [Message] = [
            makeMsg(serverU1, .user, "First question"),
            makeMsg(localA1, .herald, "First answer", jobID: job1,
                    reasoning: "Thinking…",
                    toolActivities: [ToolActivity(label: "findmy-family"),
                                     ToolActivity(label: "bash")]),
            makeMsg(localU2, .user, "Second question", jobID: job2),
            makeMsg(placeholder, .herald, "", jobID: job2, isStreaming: true),
        ]

        let refreshed: [Message] = [
            makeMsg(serverU1, .user, "First question"),
            makeMsg(serverA1, .herald, "First answer", jobID: job1),
            makeMsg(serverU2, .user, "Second question"),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        let answer = merged.first { $0.content == "First answer" }
        #expect(answer?.toolActivities.count == 2, "tool timeline lost on refresh")
        #expect(answer?.reasoning.isEmpty == false, "reasoning lost on refresh")
    }

    @Test("A prompt the server has not persisted yet keeps its place")
    func unpersistedPromptKeepsItsPlace() {
        // The other timing: the poll fires before Hermes has written the user
        // turn, so the prompt is genuinely local-only and must splice in after
        // the previous reply — still above its own placeholder.
        let serverU1 = UUID(), localA1 = UUID(), localU2 = UUID(), placeholder = UUID()
        let serverA1 = UUID()
        let job1 = UUID(), job2 = UUID()

        let local: [Message] = [
            makeMsg(serverU1, .user, "First question"),
            makeMsg(localA1, .herald, "First answer", jobID: job1),
            makeMsg(localU2, .user, "Second question", jobID: job2),
            makeMsg(placeholder, .herald, "", jobID: job2, isStreaming: true),
        ]

        let refreshed: [Message] = [
            makeMsg(serverU1, .user, "First question"),
            makeMsg(serverA1, .herald, "First answer", jobID: job1),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        #expect(order(merged) == [
            "user:First question",
            "herald:First answer",
            "user:Second question",
            "herald:<streaming>",
        ], "got \(order(merged))")
    }
}
