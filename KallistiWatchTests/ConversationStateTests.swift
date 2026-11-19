import Foundation
import XCTest

@MainActor
final class ConversationStateTests: XCTestCase {
    private let chooser = WatchTransportChooser(relay: nil, direct: nil)

    /// Every state the orb can be in, in the order a turn walks them.
    func testTheTurnWalksEveryStateOnce() {
        let store = WatchConversationStore()
        XCTAssertEqual(store.state, .idle)

        store.begin()
        XCTAssertEqual(store.state, .listening)
        XCTAssertTrue(store.state.listens)

        // A tool call is the longest silence in a turn, so it is a state with a
        // name, not a spinner.
        store.apply(.tool(name: "pytest"))
        XCTAssertEqual(store.state, .thinking(tool: "pytest"))
        XCTAssertTrue(store.state.word.contains("pytest"))

        store.apply(.delta(text: "The sea is cold."))
        XCTAssertEqual(store.state, .speaking)

        store.bargeIn()
        XCTAssertEqual(store.state, .interrupted, "the barge-in is its own state")
        store.resumeListening()
        XCTAssertEqual(store.state, .listening, "and the mic opens only when asked")

        store.apply(.done(text: "The sea is cold."))
        XCTAssertEqual(store.state, .idle)
    }

    /// A lower wrist keeps the audio and stops the mic. The two are not the same
    /// gesture and must not be treated as one.
    func testALoweredWristStopsTheMicAndKeepsTheAudio() {
        let store = WatchConversationStore()
        store.begin()
        store.wristLowered()
        XCTAssertEqual(store.state, .thinking(tool: nil), "listening ends on a lowered wrist")

        store.apply(.delta(text: "Still speaking."))
        store.wristLowered()
        XCTAssertEqual(store.state, .speaking, "speaking survives a lowered wrist")
    }

    /// A failure states the carrier and its own word, never a generic sentence.
    func testAFailureNamesTheCarrierAndTheReason() {
        let store = WatchConversationStore()
        store.apply(.failed(code: "not_ready", message: ""))
        guard case let .failed(reason) = store.state else {
            return XCTFail("a failed frame must land in failed, got \(store.state)")
        }
        XCTAssertTrue(reason.contains("turn_failed"), reason)
    }

    /// Delta text lands in one assistant turn, not one per fragment.
    func testDeltasAccumulateIntoOneTurn() {
        let store = WatchConversationStore()
        store.begin()
        store.apply(.delta(text: "One. "))
        store.apply(.delta(text: "Two."))
        XCTAssertEqual(store.turns.count, 1)
        XCTAssertEqual(store.turns.first?.text, "One. Two.")
    }
}

@MainActor
final class LaunchStateTests: XCTestCase {
    /// The URL a complication taps opens the conversation, and a URL that is not
    /// ours does nothing.
    func testTheTalkURLAsksForTheConversation() {
        let launch = WatchLaunchState()
        launch.handle(URL(string: "kallisti://talk")!)
        XCTAssertTrue(launch.openConversation)
        XCTAssertFalse(launch.startListening, "a tap opens the screen, not the mic")

        let untouched = WatchLaunchState()
        untouched.handle(URL(string: "https://example.test/talk")!)
        XCTAssertFalse(untouched.openConversation)
    }

    /// The intent asks for the mic as well as the screen.
    func testTheIntentAsksForTheMic() {
        let launch = WatchLaunchState()
        launch.open(listening: true)
        XCTAssertTrue(launch.openConversation)
        XCTAssertTrue(launch.startListening)
    }
}

/// A counter that survives being called from an `@Sendable` closure.
private actor CallCounter {
    private(set) var value = 0

    func bump() {
        value += 1
    }
}

final class WatchAuthTests: XCTestCase {
    private func paired() throws -> WatchPairingStore {
        let store = WatchPairingStore(store: MemorySecureStore())
        try store.save(
            WatchCredentials(
                accessToken: "old",
                refreshToken: "refresh-1",
                deviceID: "watch-1",
                origin: nil
            )
        )
        return store
    }

    /// A pairing survives a store with no cache, which is what a relaunch is.
    func testAPairingRoundTripsThroughTheStore() throws {
        let memory = MemorySecureStore()
        let first = WatchPairingStore(store: memory)
        XCTAssertNil(try first.credentials())
        try first.save(
            WatchCredentials(
                accessToken: "a",
                refreshToken: "r",
                deviceID: "d",
                origin: URL(string: "http://127.0.0.1:7341")
            )
        )
        let second = WatchPairingStore(store: memory)
        XCTAssertEqual(try second.credentials()?.accessToken, "a")
    }

    /// One 401 is one refresh, and the retried request carries the new token.
    func testOneRefresOnOneUnauthorised() async throws {
        let store = try paired()
        let counter = CallCounter()
        let auth = WatchAuth(
            refresh: { _ in
                await counter.bump()
                return WatchCredentials(
                    accessToken: "new",
                    refreshToken: "refresh-2",
                    deviceID: "watch-1",
                    origin: nil
                )
            },
            store: store
        )
        let used = try await auth.run { token in
            if token == "old" { throw WatchTransportError.refused(status: 401, code: nil) }
            return token
        }
        XCTAssertEqual(used, "new")
        let calls = await counter.value
        XCTAssertEqual(calls, 1, "one 401 is one refresh")
    }

    /// A second 401 is the end: the wrist asks to be paired again rather than
    /// refreshing forever.
    func testASecondUnauthorisedIsAReauth() async throws {
        let store = try paired()
        let counter = CallCounter()
        let auth = WatchAuth(
            refresh: { _ in
                await counter.bump()
                return WatchCredentials(
                    accessToken: "still-refused",
                    refreshToken: "refresh-2",
                    deviceID: "watch-1",
                    origin: nil
                )
            },
            store: store
        )
        do {
            _ = try await auth.run { _ in
                throw WatchTransportError.refused(status: 401, code: nil)
            }
            XCTFail("two 401s must not succeed")
        } catch let error as WatchAuthError {
            XCTAssertEqual(error, .reauth)
        }
    }

    /// No pairing at all is its own word, not a network failure.
    func testNoPairingIsNotPaired() async throws {
        let store = WatchPairingStore(store: MemorySecureStore())
        let auth = WatchAuth(refresh: { _ in throw WatchAuthError.notPaired }, store: store)
        do {
            _ = try await auth.run { _ in "unreachable" }
            XCTFail("a run with no pairing must not succeed")
        } catch let error as WatchAuthError {
            XCTAssertEqual(error, .notPaired)
        }
    }
}
