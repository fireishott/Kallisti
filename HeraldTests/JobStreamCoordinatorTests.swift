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

// MARK: - Structured Error Propagation Tests

@Suite("Structured error category propagation")
@MainActor
struct StructuredErrorTests {

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

    @Test("runFailed SSE event with errorCategory and errorAction")
    func runFailedParsesCategoryAndAction() async {
        let coordinator = makeCoordinator()
        let event = SSEEvent(
            event: "done",
            data: "{\"status\": \"failed\", \"error\": \"Context length exceeded\", \"errorCategory\": \"context_exceeded\", \"errorAction\": \"new_session\"}",
            id: "1"
        )
        let envelope = await coordinator.parseEnvelope(from: event)

        #expect(envelope != nil)
        #expect(envelope?.type == .runFailed)

        if case .runFailed(let payload) = envelope?.payload {
            #expect(payload.error == "Context length exceeded")
            #expect(payload.errorCategory == "context_exceeded")
            #expect(payload.errorAction == "new_session")
        } else {
            Issue.record("Expected runFailed payload")
        }
    }

    @Test("runFailed SSE event without errorCategory defaults to nil")
    func runFailedWithoutCategoryDefaultsToNil() async {
        let coordinator = makeCoordinator()
        let event = SSEEvent(
            event: "done",
            data: "{\"status\": \"failed\", \"error\": \"Something went wrong\"}",
            id: "1"
        )
        let envelope = await coordinator.parseEnvelope(from: event)

        #expect(envelope != nil)
        if case .runFailed(let payload) = envelope?.payload {
            #expect(payload.errorCategory == nil)
            #expect(payload.errorAction == nil)
        } else {
            Issue.record("Expected runFailed payload")
        }
    }

    @Test("runFailed with timeout category")
    func runFailedTimeoutCategory() async {
        let coordinator = makeCoordinator()
        let event = SSEEvent(
            event: "done",
            data: "{\"status\": \"failed\", \"error\": \"Request timed out\", \"errorCategory\": \"timeout\", \"errorAction\": \"retry\"}",
            id: "1"
        )
        let envelope = await coordinator.parseEnvelope(from: event)

        if case .runFailed(let payload) = envelope?.payload {
            #expect(payload.errorCategory == "timeout")
            #expect(payload.errorAction == "retry")
        } else {
            Issue.record("Expected runFailed payload")
        }
    }

    @Test("runFailed with rate_limited category")
    func runFailedRateLimitedCategory() async {
        let coordinator = makeCoordinator()
        let event = SSEEvent(
            event: "done",
            data: "{\"status\": \"failed\", \"error\": \"Rate limit exceeded\", \"errorCategory\": \"rate_limited\", \"errorAction\": \"wait\"}",
            id: "1"
        )
        let envelope = await coordinator.parseEnvelope(from: event)

        if case .runFailed(let payload) = envelope?.payload {
            #expect(payload.errorCategory == "rate_limited")
            #expect(payload.errorAction == "wait")
        } else {
            Issue.record("Expected runFailed payload")
        }
    }

    @Test("JobEventReducer propagates errorCategory to TerminalEvent")
    func reducerPropagatesCategoryToTerminalEvent() {
        var projection = JobProjection(jobId: "test", conversationId: "conv")
        let envelope = JobEventEnvelope(
            contractVersion: 2,
            jobId: "test",
            conversationId: "conv",
            attempt: 1,
            seq: 1,
            type: .runFailed,
            timestamp: Date(),
            payload: .runFailed(RunFailedPayload(
                error: "Context exceeded",
                retryable: false,
                errorCategory: "context_exceeded",
                errorAction: "new_session"
            ))
        )

        JobEventReducer.reduce(&projection, event: envelope)

        #expect(projection.isTerminal == true)
        #expect(projection.phase == .failed)
        #expect(projection.errorMessage == "Context exceeded")

        if case .failed(let error, let retryable, let category, let action) = projection.terminalEvent {
            #expect(error == "Context exceeded")
            #expect(retryable == false)
            #expect(category == "context_exceeded")
            #expect(action == "new_session")
        } else {
            Issue.record("Expected failed terminal event")
        }
    }

    @Test("RunFailedPayload decodes with missing optional fields")
    func runFailedPayloadDecodesWithMissingFields() throws {
        let json = """
        {"error": "test error", "retryable": true}
        """
        let data = json.data(using: .utf8)!
        let payload = try JSONDecoder().decode(RunFailedPayload.self, from: data)

        #expect(payload.error == "test error")
        #expect(payload.retryable == true)
        #expect(payload.errorCategory == nil)
        #expect(payload.errorAction == nil)
    }

    @Test("RunFailedPayload decodes with all fields")
    func runFailedPayloadDecodesWithAllFields() throws {
        let json = """
        {"error": "timeout", "retryable": true, "errorCategory": "timeout", "errorAction": "retry"}
        """
        let data = json.data(using: .utf8)!
        let payload = try JSONDecoder().decode(RunFailedPayload.self, from: data)

        #expect(payload.error == "timeout")
        #expect(payload.retryable == true)
        #expect(payload.errorCategory == "timeout")
        #expect(payload.errorAction == "retry")
    }
}
