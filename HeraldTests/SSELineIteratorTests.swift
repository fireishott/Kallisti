import Foundation
import Testing
@testable import Herald

// MARK: - SSELineIterator Tests
//
// These tests verify that our custom SSE line iterator preserves empty lines,
// which is critical for SSE event dispatch. Foundation's AsyncLineSequence
// has been observed to silently drop empty strings — the exact bug fixed in
// dochi PR #388.

@Suite("SSELineIterator — empty line preservation")
struct SSELineIteratorTests {

    /// Simulates an async byte stream from a `Data` input, matching the
    /// behavior of URLSession.AsyncBytes (one byte at a time).
    private static func makeAsyncBytes(from data: Data) -> AsyncThrowingStream<UInt8, Error> {
        AsyncThrowingStream { continuation in
            for byte in data {
                continuation.yield(byte)
            }
            continuation.finish()
        }
    }

    /// Helper: collect all lines from an SSE byte stream.
    private static func collectLines(from sseData: Data) async throws -> [String] {
        // Wrap our mock bytes in a type that provides the sseLines extension.
        // Since sseLines is defined on URLSession.AsyncBytes and we can't
        // instantiate that directly, we test drainSSELines directly instead.
        fatalError("Use drainSSELines-based tests below")
    }

    // MARK: - drainSSELines unit tests (the core logic)

    @Test("Single empty line from consecutive newlines")
    func singleEmptyLineFromConsecutiveNewlines() {
        // Input: "id: 1\nevent: text_delta\ndata: {\"delta\":\"Hi\"}\n\n"
        // The \n\n at the end should produce an empty string.
        let input = Data("id: 1\nevent: text_delta\ndata: {\"delta\":\"Hi\"}\n\n".utf8)
        var buffer = input
        let lines = drainSSELines(from: &buffer)
        #expect(lines == ["id: 1", "event: text_delta", "data: {\"delta\":\"Hi\"}", ""])
        #expect(buffer.isEmpty)
    }

    @Test("Two consecutive events with empty-line delimiter")
    func twoConsecutiveEvents() {
        // Two complete SSE frames separated by \n\n.
        // Frame 1: id: 1\nevent: text_delta\ndata: {"delta":"Hello"}\n\n
        // Frame 2: id: 2\nevent: text_delta\ndata: {"delta":"World"}\n\n
        let input = Data(
            "id: 1\nevent: text_delta\ndata: {\"delta\":\"Hello\"}\n\n" +
            "id: 2\nevent: text_delta\ndata: {\"delta\":\"World\"}\n\n".utf8
        )
        var buffer = input
        let lines = drainSSELines(from: &buffer)
        #expect(lines == [
            "id: 1",
            "event: text_delta",
            "data: {\"delta\":\"Hello\"}",
            "",           // ← critical: empty line dispatches first event
            "id: 2",
            "event: text_delta",
            "data: {\"delta\":\"World\"}",
            "",           // ← critical: empty line dispatches second event
        ])
        #expect(buffer.isEmpty)
    }

    @Test("SSE comment keepalive is preserved as line")
    func commentKeepalive() {
        // Relay sends ": keepalive\n\n" every 30s.
        let input = Data(": keepalive\n\n".utf8)
        var buffer = input
        let lines = drainSSELines(from: &buffer)
        // The SSE parser checks line.hasPrefix(":") to skip comments.
        // We just need the empty line at the end to be preserved.
        #expect(lines == [": keepalive", ""])
        #expect(buffer.isEmpty)
    }

    @Test("Partial line (no trailing newline) stays in buffer")
    func partialLineStaysInBuffer() {
        // Simulates a chunked delivery: "id: 1\nevent: text_delta\nda"
        // where "ta: ..." hasn't arrived yet.
        let input = Data("id: 1\nevent: text_delta\nda".utf8)
        var buffer = input
        let lines = drainSSELines(from: &buffer)
        #expect(lines == ["id: 1", "event: text_delta"])
        // "da" stays — no newline yet
        #expect(buffer == Data("da".utf8))
    }

    @Test("Buffer with only partial data yields nothing")
    func partialDataOnly() {
        var buffer = Data("no_newline_yet".utf8)
        let lines = drainSSELines(from: &buffer)
        #expect(lines == [])
        #expect(buffer == Data("no_newline_yet".utf8))
    }

    @Test("Empty buffer yields nothing")
    func emptyBuffer() {
        var buffer = Data()
        let lines = drainSSELines(from: &buffer)
        #expect(lines == [])
        #expect(buffer.isEmpty)
    }

    @Test("\\r\\n line endings are normalized")
    func crlfLineEndings() {
        // Some servers send \r\n instead of \n.
        let input = Data("id: 1\r\nevent: text_delta\r\ndata: {}\r\n\r\n".utf8)
        var buffer = input
        let lines = drainSSELines(from: &buffer)
        #expect(lines == ["id: 1", "event: text_delta", "data: {}", ""])
        #expect(buffer.isEmpty)
    }

    @Test("Multiple consecutive empty lines preserved")
    func multipleConsecutiveEmptyLines() {
        // \n\n\n should produce two empty strings.
        let input = Data("data: {}\n\n\n".utf8)
        var buffer = input
        let lines = drainSSELines(from: &buffer)
        #expect(lines == ["data: {}", "", ""])
        #expect(buffer.isEmpty)
    }
}
