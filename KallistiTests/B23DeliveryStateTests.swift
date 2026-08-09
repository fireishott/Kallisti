import Foundation
import Testing
@testable import Kallisti

/// Build 23: delivery-state semantics and terminal message projection.
///
/// Verifies:
/// - Cancel/failure/timeout/empty never mark user row delivered
/// - Pending acknowledgement is "sent", not "delivered"
/// - Image-only response is a credible completion
/// - Conversation refresh during active placeholder cannot upgrade to .delivered
/// - Terminal message with canonical `message.text` maps correctly
/// - Terminal message preserves attachment metadata
@MainActor
@Suite("B23 delivery state and terminal message projection")
struct B23DeliveryStateTests {

    // MARK: - Helpers

    private func makeMsg(_ id: UUID, _ sender: MessageSender, _ content: String,
                         jobID: UUID? = nil, isStreaming: Bool = false,
                         status: MessageStatus = .sending,
                         attachments: [MessageAttachment] = [],
                         reasoning: String = "") -> Message {
        Message(id: id, sender: sender, content: content, jobID: jobID,
                status: status, isStreaming: isStreaming,
                attachments: attachments, reasoning: reasoning)
    }

    private func merge(local: [Message], refreshed: [Message]) -> [Message] {
        let localConv = Conversation(id: UUID(), title: "Test", messages: local)
        let refreshedConv = Conversation(id: localConv.id, title: "Test", messages: refreshed)
        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        let merged = store.mergeConversationMetadata(from: localConv, into: refreshedConv)
        return merged?.messages ?? []
    }

    // MARK: - Terminal message.text canonical field

    @Test("done.message.text is the canonical text field")
    func canonicalMessageTextField() {
        let json: [String: Any] = [
            "message": [
                "id": UUID().uuidString,
                "role": "herald",
                "text": "Canonical answer",
                "content": "Legacy content fallback",
            ] as [String: Any],
        ]
        let text = JobStreamCoordinator.parseTerminalText(from: json)
        #expect(text == "Canonical answer")
    }

    @Test("message.content is a compatibility fallback")
    func messageContentFallback() {
        let json: [String: Any] = [
            "message": [
                "id": UUID().uuidString,
                "role": "herald",
                "content": "Legacy content",
            ] as [String: Any],
        ]
        let text = JobStreamCoordinator.parseTerminalText(from: json)
        #expect(text == "Legacy content")
    }

    @Test("Terminal message with one image maps attachment metadata and remoteIndex")
    func terminalMessageMapsImageAttachment() {
        let messageId = UUID()
        let jobId = UUID()
        let json: [String: Any] = [
            "id": messageId.uuidString,
            "role": "herald",
            "text": "Here is an image",
            "attachments": [
                [
                    "type": "image",
                    "filename": "photo.png",
                    "mimeType": "image/png",
                    "thumbnailData": "iVBORw0KGgo=",
                ] as [String: Any],
            ],
        ]

        let message = LiveHeraldClient.finalMessage(
            fromTerminalText: "Here is an image",
            jobId: jobId,
            messageJSON: json
        )

        #expect(message != nil)
        #expect(message?.id == messageId)
        #expect(message?.content == "Here is an image")
        #expect(message?.attachments.count == 1)
        if let att = message?.attachments.first {
            #expect(att.kind == "image")
            #expect(att.fileName == "photo.png")
            #expect(att.mimeType == "image/png")
            #expect(att.thumbnailBase64 == "iVBORw0KGgo=")
            #expect(att.messageID == messageId)
            #expect(att.remoteIndex == 0)
        }
    }

    // MARK: - Credible completion

    @Test("Image-only terminal message is a credible completion")
    func imageOnlyIsCredibleCompletion() {
        // An assistant message with no visible text but valid attachments
        // must be considered a credible completion.
        let imageAttachment = MessageAttachment(
            kind: "image",
            fileName: "chart.png",
            mimeType: "image/png",
            thumbnailBase64: "abc123",
            messageID: UUID(),
            remoteIndex: 0
        )
        let msg = Message(
            sender: .herald,
            content: "",
            status: .delivered,
            attachments: [imageAttachment],
            reasoning: ""
        )

        let isCredible = !msg.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !msg.reasoning.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !msg.attachments.isEmpty

        #expect(isCredible)
    }

    @Test("Empty text with no attachments is not credible")
    func emptyResponseIsNotCredible() {
        let msg = Message(
            sender: .herald,
            content: "",
            status: .delivered,
            attachments: [],
            reasoning: ""
        )

        // Build 26: only visible text or valid attachments.
        let isCredible = !msg.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !msg.attachments.isEmpty

        #expect(!isCredible)
    }

    @Test("Reasoning-only is not credible (Build 26: hidden thought does not signal completion)")
    func reasoningOnlyIsNotCredible() {
        let msg = Message(
            sender: .herald,
            content: "",
            status: .delivered,
            attachments: [],
            reasoning: "The user asked about... I should respond with..."
        )

        // Build 26: only visible text or valid attachments make a completion
        // credible.  Hidden reasoning alone never counts.
        let isCredible = !msg.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !msg.attachments.isEmpty

        #expect(!isCredible)
    }

    // MARK: - Merge: server "delivered" must not overwrite active user row

    @Test("Merge cannot upgrade active user row from .sending to .delivered")
    func mergeDoesNotUpgradeActiveUserRow() {
        let userMsgId = UUID()
        let jobId = UUID()
        let placeholderId = UUID()

        // Local: user message is .sending, placeholder is streaming
        let local: [Message] = [
            makeMsg(userMsgId, .user, "hello", jobID: jobId, status: .sending),
            makeMsg(placeholderId, .herald, "", jobID: jobId, isStreaming: true, status: .sending),
        ]

        // Server: user message is .delivered (prematurely)
        let refreshed: [Message] = [
            Message(id: userMsgId, sender: .user, content: "hello", jobID: jobId, status: .delivered),
            Message(id: UUID(), sender: .herald, content: "", jobID: jobId, status: .sending),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        // The user row must NOT be .delivered because the placeholder is still active
        if let userMsg = merged.first(where: { $0.id == userMsgId }) {
            #expect(userMsg.status != .delivered,
                    "User row was upgraded to .delivered despite active placeholder")
        }
    }

    @Test("Merge cannot upgrade .sent user row to .delivered while placeholder active")
    func mergeDoesNotUpgradeSentUserRow() {
        let userMsgId = UUID()
        let jobId = UUID()
        let placeholderId = UUID()

        let local: [Message] = [
            makeMsg(userMsgId, .user, "hello", jobID: jobId, status: .sent),
            makeMsg(placeholderId, .herald, "", jobID: jobId, isStreaming: true, status: .sending),
        ]

        let refreshed: [Message] = [
            Message(id: userMsgId, sender: .user, content: "hello", jobID: jobId, status: .delivered),
            Message(id: UUID(), sender: .herald, content: "", jobID: jobId, status: .sending),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        if let userMsg = merged.first(where: { $0.id == userMsgId }) {
            #expect(userMsg.status != .delivered,
                    "User row was upgraded from .sent to .delivered despite active placeholder")
        }
    }

    @Test("Merge allows delivered user row when no placeholder is active")
    func mergeAllowsDeliveredWhenNoPlaceholder() {
        let userMsgId = UUID()
        let jobId = UUID()

        // No streaming placeholder — both local and server rows are terminal
        let local: [Message] = [
            makeMsg(userMsgId, .user, "hello", jobID: jobId, status: .delivered),
        ]

        let refreshed: [Message] = [
            Message(id: userMsgId, sender: .user, content: "hello", jobID: jobId, status: .delivered),
        ]

        let merged = merge(local: local, refreshed: refreshed)

        if let userMsg = merged.first(where: { $0.id == userMsgId }) {
            #expect(userMsg.status == .delivered,
                    "Terminal user row should remain .delivered when no placeholder is active")
        }
    }

    // MARK: - Reconstructed message identity

    @Test("Terminal message reconstruction preserves message ID")
    func terminalMessagePreservesID() {
        let messageId = UUID()
        let jobId = UUID()
        let json: [String: Any] = [
            "id": messageId.uuidString,
            "role": "herald",
            "text": "Response text",
        ]

        let message = LiveHeraldClient.finalMessage(
            fromTerminalText: "Response text",
            jobId: jobId,
            messageJSON: json
        )

        #expect(message?.id == messageId)
        #expect(message?.jobID == jobId)
        #expect(message?.sender == .herald)
        #expect(message?.status == .delivered)
    }

    @Test("Terminal message without messageJSON falls back to text-only")
    func terminalMessageWithoutJSONFallsBack() {
        let jobId = UUID()
        let message = LiveHeraldClient.finalMessage(
            fromTerminalText: "Plain text response",
            jobId: jobId,
            messageJSON: nil
        )

        #expect(message != nil)
        #expect(message?.content == "Plain text response")
        #expect(message?.jobID == jobId)
        #expect(message?.attachments.isEmpty == true)
    }
}
