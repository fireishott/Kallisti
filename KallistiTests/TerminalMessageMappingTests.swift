import Foundation
import Testing
@testable import Kallisti

@MainActor
@Suite("Terminal text maps to the final message")
struct TerminalMessageMappingTests {

    @Test("Non-empty terminal text becomes a delivered herald message")
    func nonEmptyTextBecomesMessage() {
        let jobId = UUID()
        let message = LiveHeraldClient.finalMessage(fromTerminalText: "CADDY OK", jobId: jobId)
        #expect(message?.content == "CADDY OK")
        #expect(message?.sender == .herald)
        #expect(message?.jobID == jobId)
        #expect(message?.status == .delivered)
    }

    @Test("Nil terminal text produces no message")
    func nilTextProducesNil() {
        #expect(LiveHeraldClient.finalMessage(fromTerminalText: nil, jobId: UUID()) == nil)
    }

    @Test("Empty and whitespace-only terminal text produce no message")
    func blankTextProducesNil() {
        #expect(LiveHeraldClient.finalMessage(fromTerminalText: "", jobId: UUID()) == nil)
        #expect(LiveHeraldClient.finalMessage(fromTerminalText: "   \n ", jobId: UUID()) == nil)
    }
}
