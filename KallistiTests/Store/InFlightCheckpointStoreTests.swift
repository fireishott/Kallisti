import Foundation
import Testing
@testable import Kallisti

/// Regression tests for InFlightCheckpoint (Codable round-trip) and
/// InFlightCheckpointStore (UserDefaults persistence).
@MainActor
@Suite("InFlightCheckpointStore")
struct InFlightCheckpointStoreTests {

    // MARK: - InFlightCheckpoint encode / decode

    @Test("Checkpoint round-trips through JSONEncoder/JSONDecoder")
    func checkpointRoundTrip() throws {
        let checkpoint = InFlightCheckpoint(
            conversationID: UUID(),
            nativeSessionID: "session-abc-123",
            jobID: UUID(),
            backgroundedAt: Date(timeIntervalSince1970: 1_700_000_000)
        )

        let data = try JSONEncoder().encode(checkpoint)
        let decoded = try JSONDecoder().decode(InFlightCheckpoint.self, from: data)

        #expect(decoded == checkpoint)
        #expect(decoded.conversationID == checkpoint.conversationID)
        #expect(decoded.nativeSessionID == "session-abc-123")
        #expect(decoded.jobID == checkpoint.jobID)
        #expect(decoded.backgroundedAt == checkpoint.backgroundedAt)
    }

    @Test("Checkpoint with nil jobID encodes and decodes correctly")
    func checkpointNilJobID() throws {
        let checkpoint = InFlightCheckpoint(
            conversationID: UUID(),
            nativeSessionID: "no-job",
            jobID: nil,
            backgroundedAt: Date()
        )

        let data = try JSONEncoder().encode(checkpoint)
        let decoded = try JSONDecoder().decode(InFlightCheckpoint.self, from: data)

        #expect(decoded.jobID == nil)
        #expect(decoded.conversationID == checkpoint.conversationID)
    }

    // MARK: - InFlightCheckpointStore round-trip via UserDefaults.standard

    @Test("Save then load returns the same checkpoint")
    func saveAndLoad() {
        let checkpoint = InFlightCheckpoint(
            conversationID: UUID(),
            nativeSessionID: "persist-test",
            jobID: UUID(),
            backgroundedAt: Date()
        )

        InFlightCheckpointStore.save(checkpoint)
        let loaded = InFlightCheckpointStore.load()

        #expect(loaded == checkpoint)
        InFlightCheckpointStore.clear()
    }

    @Test("Load returns nil on fresh/cleared state")
    func loadNilOnFreshState() {
        InFlightCheckpointStore.clear()
        let loaded = InFlightCheckpointStore.load()
        #expect(loaded == nil)
    }

    @Test("Clear removes the persisted checkpoint")
    func clearRemovesCheckpoint() {
        let checkpoint = InFlightCheckpoint(
            conversationID: UUID(),
            nativeSessionID: "to-clear",
            jobID: nil,
            backgroundedAt: Date()
        )

        InFlightCheckpointStore.save(checkpoint)
        #expect(InFlightCheckpointStore.load() != nil)

        InFlightCheckpointStore.clear()
        #expect(InFlightCheckpointStore.load() == nil)
    }

    @Test("Save overwrites a previous checkpoint")
    func saveOverwrites() {
        let first = InFlightCheckpoint(
            conversationID: UUID(),
            nativeSessionID: "first",
            jobID: nil,
            backgroundedAt: Date(timeIntervalSince1970: 1_000_000)
        )
        let second = InFlightCheckpoint(
            conversationID: UUID(),
            nativeSessionID: "second",
            jobID: UUID(),
            backgroundedAt: Date(timeIntervalSince1970: 2_000_000)
        )

        InFlightCheckpointStore.save(first)
        InFlightCheckpointStore.save(second)
        let loaded = InFlightCheckpointStore.load()

        #expect(loaded == second)
        #expect(loaded?.nativeSessionID == "second")
        InFlightCheckpointStore.clear()
    }
}
