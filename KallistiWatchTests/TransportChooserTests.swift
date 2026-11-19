import XCTest

/// A transport that does exactly what a closure says, so the chooser can be
/// driven through each branch without a phone or a daemon.
private struct StubTransport: WatchTransport {
    let name: TransportName
    let send: @Sendable (String) async throws -> String

    func pair(deviceName: String) async throws -> WatchPairing {
        throw WatchTransportError.refused(status: 501, code: "pair_from_phone")
    }

    func sendTurn(text: String, conversationId: UUID?) async throws -> String {
        try await send(text)
    }

    func events(jobID: String, audio: Bool) -> AsyncThrowingStream<WatchFrame, Error> {
        AsyncThrowingStream { $0.finish(throwing: WatchTransportError.framesUnavailable) }
    }

    func speak(text: String, voice: String?) async throws -> Data {
        throw WatchTransportError.framesUnavailable
    }

    func transcribe(audio: Data) async throws -> String {
        throw WatchTransportError.framesUnavailable
    }

    func cancel(jobID: String) async throws {}
}

final class TransportChooserTests: XCTestCase {
    /// The relay answers inside the window: it carries the turn and the screen
    /// says so.
    func testRelayAcknowledgementCarriesTheTurn() async throws {
        let relay = StubTransport(name: .relay) { _ in "relay-handle" }
        let direct = StubTransport(name: .direct) { _ in
            XCTFail("the direct client must not be touched when the relay answers")
            return "direct-handle"
        }
        let chooser = WatchTransportChooser(
            relay: relay,
            direct: direct,
            acknowledgementTimeout: .milliseconds(50)
        )
        let result = try await chooser.sendTurn(text: "hello")
        XCTAssertEqual(result.carrier, .relay)
        XCTAssertEqual(result.handle, "relay-handle")
    }

    /// The relay never answers: the turn goes direct, and the reported carrier
    /// says which path actually carried it.
    func testARelayThatNeverAnswersFallsBackToDirect() async throws {
        let relay = StubTransport(name: .relay) { _ in
            try await Task.sleep(for: .seconds(10))
            return "relay-handle"
        }
        let direct = StubTransport(name: .direct) { _ in "direct-handle" }
        let chooser = WatchTransportChooser(
            relay: relay,
            direct: direct,
            acknowledgementTimeout: .milliseconds(30)
        )
        let result = try await chooser.sendTurn(text: "hello")
        XCTAssertEqual(result.carrier, .direct)
        XCTAssertEqual(result.handle, "direct-handle")
    }

    /// No relay at all: direct is the only path, so it is the one reported.
    func testWithoutARelayTheDirectClientCarriesTheTurn() async throws {
        let direct = StubTransport(name: .direct) { _ in "direct-handle" }
        let chooser = WatchTransportChooser(
            relay: nil,
            direct: direct,
            acknowledgementTimeout: .milliseconds(30)
        )
        let result = try await chooser.sendTurn(text: "hello")
        XCTAssertEqual(result.carrier, .direct)
    }

    /// Both paths fail: the direct client was tried last, so its refusal is the
    /// one reported. A screen that said "unreachable" when the daemon actually
    /// answered `401` would send a person to the wrong fix.
    func testBothPathsFailingReportsTheLastFailure() async throws {
        let relay = StubTransport(name: .relay) { _ in
            throw WatchTransportError.unreachable
        }
        let direct = StubTransport(name: .direct) { _ in
            throw WatchTransportError.refused(status: 401, code: nil)
        }
        let chooser = WatchTransportChooser(
            relay: relay,
            direct: direct,
            acknowledgementTimeout: .milliseconds(30)
        )
        do {
            _ = try await chooser.sendTurn(text: "hello")
            XCTFail("a turn with no transport must not succeed")
        } catch let error as WatchTransportError {
            XCTAssertEqual(error, .refused(status: 401, code: nil))
        }
    }
}
