import Foundation
import Testing
@testable import Herald

// MARK: - Helpers

private let fixtureDirectory = Bundle.module.url(
    forResource: "Fixtures/StreamContractV2",
    withExtension: nil
)!

private let decoder: JSONDecoder = {
    let d = JSONDecoder()
    d.dateDecodingStrategy = .iso8601
    return d
}()

private func loadFixture(named name: String) throws -> [JobEventEnvelope] {
    let url = fixtureDirectory.appendingPathComponent(name)
    let data = try Data(contentsOf: url)
    return try decoder.decode([JobEventEnvelope].self, from: data)
}

private let fixtureFiles = [
    "stream_v2_text_only.json",
    "stream_v2_reasoning.json",
    "stream_v2_multi_tools.json",
    "stream_v2_commentary.json",
    "stream_v2_approval.json",
    "stream_v2_error.json",
    "stream_v2_cancelled.json",
    "stream_v2_goal_continuation.json",
]

// MARK: - Tests

@Suite("Stream Contract v2 golden fixtures")
struct StreamContractV2Tests {

    @Test("All 8 fixture files are present", arguments: fixtureFiles)
    func fixtureParses(filename: String) throws {
        let events = try loadFixture(named: filename)
        #expect(!events.isEmpty, "Fixture \(filename) should not be empty")
    }

    @Test("contractVersion is always 2", arguments: fixtureFiles)
    func contractVersionIsTwo(filename: String) throws {
        let events = try loadFixture(named: filename)
        for event in events {
            #expect(
                event.contractVersion == 2,
                "Expected contractVersion=2, got \(event.contractVersion) (seq=\(event.seq), type=\(event.type))"
            )
        }
    }

    @Test("jobId is consistent across fixture", arguments: fixtureFiles)
    func jobIdConsistent(filename: String) throws {
        let events = try loadFixture(named: filename)
        let ids = Set(events.map(\.jobId))
        #expect(ids.count == 1, "Multiple jobIds found: \(ids)")
    }

    @Test("conversationId is consistent across fixture", arguments: fixtureFiles)
    func conversationIdConsistent(filename: String) throws {
        let events = try loadFixture(named: filename)
        let ids = Set(events.map(\.conversationId))
        #expect(ids.count == 1, "Multiple conversationIds found: \(ids)")
    }

    @Test("seq is monotonically increasing starting at 1", arguments: fixtureFiles)
    func seqMonotonicallyIncreasing(filename: String) throws {
        let events = try loadFixture(named: filename)
        let seqs = events.map(\.seq)
        #expect(seqs.first == 1, "seq should start at 1, got \(seqs.first ?? -1)")
        for i in 1..<seqs.count {
            #expect(
                seqs[i] == seqs[i - 1] + 1,
                "seq gap between \(seqs[i - 1]) and \(seqs[i])"
            )
        }
    }

    @Test("terminal or requeued event is last", arguments: fixtureFiles)
    func terminalOrRequeuedIsLast(filename: String) throws {
        let events = try loadFixture(named: filename)
        let terminalIndices = events.enumerated()
            .filter { $0.element.type.isTerminal }
            .map(\.offset)
        let requeuedIndices = events.enumerated()
            .filter { $0.element.type == .runRequeued }
            .map(\.offset)

        if !requeuedIndices.isEmpty {
            #expect(
                terminalIndices.isEmpty,
                "Requeued fixture should have no terminal events"
            )
            #expect(
                events.last?.type == .runRequeued,
                "Requeued event must be last"
            )
        } else {
            #expect(
                terminalIndices.count == 1,
                "Expected 1 terminal event, got \(terminalIndices.count)"
            )
            if let terminalIndex = terminalIndices.first {
                #expect(
                    terminalIndex == events.count - 1,
                    "Terminal event must be last (at index \(terminalIndex), last is \(events.count - 1))"
                )
            }
        }
    }

    @Test("attempt in terminal matches run.started", arguments: fixtureFiles)
    func attemptConsistent(filename: String) throws {
        let events = try loadFixture(named: filename)
        guard let started = events.first(where: { $0.type == .runStarted }) else {
            Issue.record("Fixture must have a run.started event")
            return
        }
        let last = events.last!
        if last.type != .runRequeued {
            #expect(
                last.attempt == started.attempt,
                "Terminal attempt (\(last.attempt)) must match run.started attempt (\(started.attempt))"
            )
        }
    }

    @Test("fixture has 3-8 events", arguments: fixtureFiles)
    func eventCountInRange(filename: String) throws {
        let events = try loadFixture(named: filename)
        #expect(
            events.count >= 3 && events.count <= 8,
            "Expected 3-8 events, got \(events.count)"
        }
    }
}
