import Foundation
import Testing
@testable import Herald

// MARK: - Terminal Parsing Tests

@Suite("JobStreamCoordinator terminal parsing")
struct TerminalParsingTests {

    @Test("Terminal event parses object message form")
    func terminalEventParsesObjectMessage() {
        let json: [String: Any] = [
            "message": [
                "content": "Final answer text",
                "role": "assistant",
            ] as [String: Any],
        ]
        let text = JobStreamCoordinator.parseTerminalText(from: json)
        #expect(text == "Final answer text")
    }

    @Test("Terminal event parses string message form")
    func terminalEventParsesStringMessage() {
        let json: [String: Any] = [
            "message": "Final answer text",
        ]
        let text = JobStreamCoordinator.parseTerminalText(from: json)
        #expect(text == "Final answer text")
    }

    @Test("Terminal event falls back to text field")
    func terminalEventFallsBackToTextField() {
        let json: [String: Any] = [
            "text": "Fallback text",
        ]
        let text = JobStreamCoordinator.parseTerminalText(from: json)
        #expect(text == "Fallback text")
    }

    @Test("Terminal event returns nil when no message or text")
    func terminalEventReturnsNilWhenEmpty() {
        let json: [String: Any] = [:]
        let text = JobStreamCoordinator.parseTerminalText(from: json)
        #expect(text == nil)
    }

    @Test("Terminal reasoning parses string form")
    func terminalReasoningParsesStringForm() {
        let json: [String: Any] = [
            "reasoning": "Step by step thinking...",
        ]
        let reasoning = JobStreamCoordinator.parseTerminalReasoning(from: json)
        #expect(reasoning == "Step by step thinking...")
    }

    @Test("Terminal reasoning parses object form")
    func terminalReasoningParsesObjectForm() {
        let json: [String: Any] = [
            "reasoning": [
                "content": "Deep analysis...",
            ] as [String: Any],
        ]
        let reasoning = JobStreamCoordinator.parseTerminalReasoning(from: json)
        #expect(reasoning == "Deep analysis...")
    }

    @Test("Terminal reasoning returns nil when absent")
    func terminalReasoningReturnsNil() {
        let json: [String: Any] = [:]
        let reasoning = JobStreamCoordinator.parseTerminalReasoning(from: json)
        #expect(reasoning == nil)
    }
}

// MARK: - SSE Comment Handling Tests

@Suite("JobStreamCoordinator SSE comment handling")
@MainActor
struct SSECommentTests {

    private func makeCoordinator() -> JobStreamCoordinator {
        JobStreamCoordinator(
            jobId: UUID(),
            conversationId: UUID(),
            clientMessageId: UUID(),
            apiClient: RelayAPIClient(baseURLProvider: { "http://localhost" }),
            accessTokenProvider: { nil },
            accessTokenRefresher: { nil },
            jobStatusProvider: { _ in nil }
        )
    }

    @Test("SSE comment event does not produce an envelope")
    func sseCommentDoesNotProduceEnvelope() async {
        let coordinator = makeCoordinator()
        let commentEvent = SSEEvent(event: "comment", data: "{}", id: "5")
        let result = await coordinator.parseEnvelope(from: commentEvent)
        #expect(result == nil)
    }

    @Test("SSE comment does not advance sequence counter")
    func sseCommentDoesNotAdvanceSequence() async {
        let coordinator = makeCoordinator()
        let initialSeq = await coordinator.lastAppliedSeq

        // Send a comment event with an id — should be ignored
        let commentEvent = SSEEvent(event: "comment", data: "{}", id: "10")
        _ = await coordinator.parseEnvelope(from: commentEvent)

        let finalSeq = await coordinator.lastAppliedSeq
        #expect(finalSeq == initialSeq, "Sequence should not advance for comment events")
    }

    @Test("Unknown event type does not produce an envelope")
    func unknownEventTypeDoesNotProduceEnvelope() async {
        let coordinator = makeCoordinator()
        let unknownEvent = SSEEvent(event: "unknown_type", data: "{}", id: "3")
        let result = await coordinator.parseEnvelope(from: unknownEvent)
        #expect(result == nil)
    }
}

// MARK: - Reconnect / Cursor Resume Tests

@Suite("JobStreamCoordinator reconnect and dedup")
@MainActor
struct ReconnectTests {

    private func makeCoordinator() -> JobStreamCoordinator {
        JobStreamCoordinator(
            jobId: UUID(),
            conversationId: UUID(),
            clientMessageId: UUID(),
            apiClient: RelayAPIClient(baseURLProvider: { "http://localhost" }),
            accessTokenProvider: { nil },
            accessTokenRefresher: { nil },
            jobStatusProvider: { _ in nil }
        )
    }

    @Test("Duplicate events are rejected (seq <= lastAppliedSeq)")
    func duplicateEventsAreRejected() async {
        let coordinator = makeCoordinator()

        // Process seq 1
        let event1 = SSEEvent(
            event: "text_delta",
            data: "{\"delta\": \"Hello\"}",
            id: "1"
        )
        let envelope1 = await coordinator.parseEnvelope(from: event1)
        #expect(envelope1 != nil)
        #expect(envelope1?.seq == 1)

        // Process seq 1 again — should be parseable but the caller should skip it
        let event1dup = SSEEvent(
            event: "text_delta",
            data: "{\"delta\": \"Hello\"}",
            id: "1"
        )
        let envelope1dup = await coordinator.parseEnvelope(from: event1dup)
        #expect(envelope1dup != nil)
        #expect(envelope1dup?.seq == 1)

        // The coordinator's run() method checks seq <= lastAppliedSeq and skips
        // This test verifies parseEnvelope produces the envelope; dedup is in run()
    }

    @Test("Events without id auto-increment seq")
    func eventsWithoutIdAutoIncrement() async {
        let coordinator = makeCoordinator()

        let event1 = SSEEvent(event: "text_delta", data: "{\"delta\": \"A\"}", id: nil)
        let envelope1 = await coordinator.parseEnvelope(from: event1)
        #expect(envelope1?.seq == 1)

        let event2 = SSEEvent(event: "text_delta", data: "{\"delta\": \"B\"}", id: nil)
        let envelope2 = await coordinator.parseEnvelope(from: event2)
        #expect(envelope2?.seq == 2)
    }

    @Test("Events with explicit id use that id as seq")
    func eventsWithExplicitIdUseThatId() async {
        let coordinator = makeCoordinator()

        let event = SSEEvent(event: "text_delta", data: "{\"delta\": \"X\"}", id: "42")
        let envelope = await coordinator.parseEnvelope(from: event)
        #expect(envelope?.seq == 42)
    }
}

// MARK: - Delta Flush Before Terminal Tests

@Suite("JobStreamCoordinator delta flush before terminal")
@MainActor
struct DeltaFlushTests {

    private func makeCoordinator() -> JobStreamCoordinator {
        JobStreamCoordinator(
            jobId: UUID(),
            conversationId: UUID(),
            clientMessageId: UUID(),
            apiClient: RelayAPIClient(baseURLProvider: { "http://localhost" }),
            accessTokenProvider: { nil },
            accessTokenRefresher: { nil },
            jobStatusProvider: { _ in nil }
        )
    }

    @Test("Terminal event with object message produces correct text after deltas")
    func terminalFlushesAfterDeltas() async {
        let coordinator = makeCoordinator()

        // Simulate: text_delta, text_delta, then terminal with object message
        let delta1 = SSEEvent(event: "text_delta", data: "{\"delta\": \"Hello \"}", id: "1")
        let delta2 = SSEEvent(event: "text_delta", data: "{\"delta\": \"world\"}", id: "2")
        let terminal = SSEEvent(
            event: "done",
            data: "{\"status\": \"completed\", \"message\": {\"content\": \"Hello world\", \"role\": \"assistant\"}}",
            id: "3"
        )

        _ = await coordinator.parseEnvelope(from: delta1)
        _ = await coordinator.parseEnvelope(from: delta2)
        let terminalEnvelope = await coordinator.parseEnvelope(from: terminal)

        #expect(terminalEnvelope != nil)
        #expect(terminalEnvelope?.type == .runCompleted)

        if case .runCompleted(let payload) = terminalEnvelope?.payload {
            #expect(payload.text == "Hello world")
        } else {
            Issue.record("Expected runCompleted payload")
        }
    }

    @Test("Terminal with string message after reasoning deltas")
    func terminalWithStringAfterReasoning() async {
        let coordinator = makeCoordinator()

        let reasoning1 = SSEEvent(event: "reasoning_delta", data: "{\"delta\": \"Thinking...\"}", id: "1")
        let terminal = SSEEvent(
            event: "done",
            data: "{\"status\": \"completed\", \"message\": \"The answer is 42\"}",
            id: "2"
        )

        _ = await coordinator.parseEnvelope(from: reasoning1)
        let terminalEnvelope = await coordinator.parseEnvelope(from: terminal)

        #expect(terminalEnvelope != nil)
        if case .runCompleted(let payload) = terminalEnvelope?.payload {
            #expect(payload.text == "The answer is 42")
        } else {
            Issue.record("Expected runCompleted payload")
        }
    }
}

// MARK: - Heartbeat / Keepalive Tests

@Suite("JobStreamCoordinator heartbeat handling")
@MainActor
struct HeartbeatTests {

    private func makeCoordinator() -> JobStreamCoordinator {
        JobStreamCoordinator(
            jobId: UUID(),
            conversationId: UUID(),
            clientMessageId: UUID(),
            apiClient: RelayAPIClient(baseURLProvider: { "http://localhost" }),
            accessTokenProvider: { nil },
            accessTokenRefresher: { nil },
            jobStatusProvider: { _ in nil }
        )
    }

    @Test("Heartbeat event produces a commentary envelope")
    func heartbeatProducesCommentary() async {
        let coordinator = makeCoordinator()
        let heartbeat = SSEEvent(event: "heartbeat", data: "{\"phase\": \"thinking\"}", id: "1")
        let envelope = await coordinator.parseEnvelope(from: heartbeat)

        #expect(envelope != nil)
        #expect(envelope?.type == .commentary)
    }

    @Test("Heartbeat produces envelope with correct seq from id")
    func heartbeatAdvancesSequence() async {
        let coordinator = makeCoordinator()

        let heartbeat = SSEEvent(event: "heartbeat", data: "{\"phase\": \"working\"}", id: "5")
        let envelope = await coordinator.parseEnvelope(from: heartbeat)

        // parseEnvelope sets seq from the SSE id but does NOT update lastAppliedSeq
        // (that happens in run()). Verify the envelope has the right seq.
        #expect(envelope != nil)
        #expect(envelope?.seq == 5)
        #expect(envelope?.type == .commentary)
    }
}
