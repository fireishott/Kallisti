import Foundation
import SwiftUI
import Testing
import UIKit
@testable import Kallisti

@MainActor
@Suite("Build 114 regressions")
struct Build114RegressionTests {
    private func message(
        _ id: UUID,
        _ sender: MessageSender,
        _ content: String,
        clientMessageID: UUID? = nil
    ) -> Message {
        Message(id: id, clientMessageID: clientMessageID, sender: sender, content: content)
    }

    @Test("Local row without a predecessor stays before its represented successor")
    func leadingLocalOnlyRowUsesSuccessorAnchor() {
        let oldPrompt = UUID()
        let representedAnswer = UUID()
        let newerPrompt = UUID()
        let newerAnswer = UUID()

        let local = Conversation(id: UUID(), title: "Test", messages: [
            message(oldPrompt, .user, "Did you fix it?"),
            message(representedAnswer, .herald, "Working on the probe"),
            message(newerPrompt, .user, "Newer prompt"),
            message(newerAnswer, .herald, "Newer answer"),
        ])
        let refreshed = Conversation(id: local.id, title: "Test", messages: [
            message(UUID(), .herald, "Working on the probe"),
            message(newerPrompt, .user, "Newer prompt"),
            message(newerAnswer, .herald, "Newer answer"),
        ])

        let store = ChatStore(heraldClient: MockHeraldClient(), persistence: MockPersistence())
        let merged = store.mergeConversationMetadata(from: local, into: refreshed)?.messages ?? []
        #expect(merged.map(\.content) == [
            "Did you fix it?", "Working on the probe", "Newer prompt", "Newer answer",
        ])
    }

    @Test("Generated thumbnail is an aspect-fill crop without white bars")
    func thumbnailHasNoLetterboxBars() throws {
        let sourceSize = CGSize(width: 60, height: 180)
        let source = UIGraphicsImageRenderer(size: sourceSize).image { context in
            UIColor.systemBlue.setFill()
            context.fill(CGRect(origin: .zero, size: sourceSize))
        }
        let data = try #require(PendingAttachment.croppedThumbnailData(from: source))
        let thumbnail = try #require(UIImage(data: data))
        let cgImage = try #require(thumbnail.cgImage)
        let providerData = try #require(cgImage.dataProvider?.data)
        let bytes = CFDataGetBytePtr(providerData)
        let bytesPerPixel = cgImage.bitsPerPixel / 8
        let bytesPerRow = cgImage.bytesPerRow

        func rgb(atX x: Int, y: Int) -> (Int, Int, Int) {
            let offset = y * bytesPerRow + x * bytesPerPixel
            return (Int(bytes![offset]), Int(bytes![offset + 1]), Int(bytes![offset + 2]))
        }

        for point in [(1, 60), (60, 60), (118, 60)] {
            let color = rgb(atX: point.0, y: point.1)
            #expect(color.2 > color.0 + 40, "expected blue image pixels at \(point), got \(color)")
        }
    }

    @Test("Composer grows through five lines then scrolls")
    func composerSizingContract() {
        #expect(PasteAwareComposerTextView.minimumHeight < PasteAwareComposerTextView.maximumHeight)
        let lineHeight = UIFont.systemFont(ofSize: 15).lineHeight
        // Build 120: default is TWO rows (avoids the 1-row -> 5-row slab).
        #expect(PasteAwareComposerTextView.minimumHeight == lineHeight * 2 + 8)
        #expect(PasteAwareComposerTextView.maximumHeight == lineHeight * 5 + 8)
    }

    @Test("Populated transcript never becomes a skeleton during refresh")
    func populatedTranscriptStaysVisible() {
        #expect(ChatScreen.shouldRedactTranscript(isLoading: true, messageCount: 0))
        #expect(!ChatScreen.shouldRedactTranscript(isLoading: true, messageCount: 1))
        #expect(!ChatScreen.shouldRedactTranscript(isLoading: false, messageCount: 0))
    }

    @Test("Active stream is always the final transcript row")
    func activeStreamOwnsTranscriptTail() {
        var streaming = message(UUID(), .herald, "current reply")
        streaming.isStreaming = true
        let staleBoundary = message(UUID(), .herald, "persisted tool boundary")
        let prompt = message(UUID(), .user, "current prompt")

        let rows = ChatScreen.transcriptRows([prompt, streaming, staleBoundary])
        #expect(rows.map(\.id) == [prompt.id, staleBoundary.id, streaming.id])
    }

    @Test("Native tool frames accept gateway float duration and full args")
    func nativeToolFramesDecodeCurrentGatewayShape() throws {
        let startData = Data(#"{"tool_id":"tc-ssh","name":"terminal","context":"ssh host","args":{"command":"ssh host uptime"}}"#.utf8)
        let start = try JSONDecoder().decode(NativeToolStartPayload.self, from: startData)
        #expect(start.toolCallID == "tc-ssh")
        #expect(start.argsText?.contains("ssh host uptime") == true)

        let completeData = Data(#"{"tool_id":"tc-edit","name":"patch","result":{"success":true},"duration_s":1.234,"inline_diff":"  ┊ review diff\na/F.swift → b/F.swift\n@@ -1 +1 @@\n-old\n+new"}"#.utf8)
        let complete = try JSONDecoder().decode(NativeToolCompletePayload.self, from: completeData)
        #expect(complete.durationMs == 1_234)
        #expect(complete.inlineDiff?.contains("a/F.swift → b/F.swift") == true)
    }
}
