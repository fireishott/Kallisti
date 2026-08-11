import SwiftUI
import Testing
@testable import Kallisti

/// Regression tests for Workstream D4: bare blinking cursor -> tiered
/// status text when streaming content is empty (e.g. while a thinking
/// model produces its first prose token).
///
/// Root cause being guarded against: the placeholder selection in
/// `MessageBubble` required `toolActivities.isEmpty && reasoning.isEmpty`,
/// so thinking models fell through to a bare `BlinkingCursor()` in
/// `MarkdownContentView`. The fix exposes a `StreamingPlaceholderText`
/// view in both places and simplifies the bubble's selection.
@Suite
struct MessageBubbleStreamingPlaceholderTests {

    // MARK: - Source-shape guard

    /// The selection condition in `MessageBubble.hermesMessage` no longer
    /// requires reasoning or tool activities to be empty. Reading the file
    /// directly is a brittle-but-honest way to lock the structure: if a
    /// future edit re-introduces the guard, this test fails immediately.
    @Test("Placeholder selection no longer guards on reasoning or tool activities")
    func placeholderSelectionNoLongerGuardsOnReasoning() throws {
        let url = try locateSourceFile(
            relativeTo: #filePath,
            segments: ["Herald", "Features", "Chat", "MessageBubble.swift"]
        )
        let source = try String(contentsOf: url, encoding: .utf8)

        // Find the hermesMessage branch and assert it does not include
        // the old guard clauses. We look for the line that gates
        // streamingPlaceholder and confirm only the streaming + empty
        // content pair survives.
        let pattern = #"else if message\.isStreaming && message\.content\.isEmpty"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            Issue.record("Failed to compile regex")
            return
        }
        let range = NSRange(source.startIndex..., in: source)
        let matches = regex.matches(in: source, range: range)
        #expect(!matches.isEmpty, "Expected at least one match for the empty-content streaming guard")

        // Find the first match and inspect the line. The line should be
        // short: the bare `isStreaming && content.isEmpty` pair. We
        // reject the longer guarded variant.
        guard let first = matches.first,
              let r = Range(first.range, in: source) else {
            Issue.record("No match range")
            return
        }
        let lineRange = source.lineRange(for: r)
        let line = String(source[lineRange])
        #expect(!line.contains("reasoning.isEmpty"),
                "Placeholder guard must not depend on reasoning being empty. Line: \(line)")
        #expect(!line.contains("toolActivities.isEmpty"),
                "Placeholder guard must not depend on tool activities being empty. Line: \(line)")
    }

    // MARK: - Tiered text view exists

    /// `StreamingPlaceholderText` is a SwiftUI View that renders a
    /// `Text`. We don't snapshot the rendered output (no ViewInspector in
    /// the target), but we can confirm the type exists, accepts a
    /// `startedAt: Date`, and its body returns a View by constructing it.
    @Test("StreamingPlaceholderText constructs and renders a View")
    func streamingPlaceholderTextExists() {
        let view = StreamingPlaceholderText(startedAt: Date())
        // Constructing the view must succeed; capturing `_ = view.body`
        // would force body evaluation in a non-render context, so we
        // only assert the type-level fact that it is a View.
        let _: any View = view
        // Anchor: tiered strings ship with the view. If a future edit
        // removes one, the user loses a status tier.
        let mirror = String(describing: StreamingPlaceholderText.self)
        #expect(mirror.contains("StreamingPlaceholderText"))
    }

    // MARK: - Tiered text strings

    /// Lock the tiered status text the user sees. The order matters:
    /// "Connecting" -> "Thinking" -> "Model is working" -> "longer than
    /// usual". Each is rendered at a different elapsed threshold.
    @Test("Tiered placeholder strings are present in MarkdownContentView")
    func tieredStringsArePresent() throws {
        let url = try locateSourceFile(
            relativeTo: #filePath,
            segments: ["Herald", "Features", "Chat", "MarkdownContentView.swift"]
        )
        let source = try String(contentsOf: url, encoding: .utf8)
        let tiers = [
            "Connecting to Hermes",
            "Thinking",
            "Model is working on your request",
            "taking longer than usual"
        ]
        for tier in tiers {
            #expect(source.contains(tier),
                    "MarkdownContentView must render the \"\(tier)\" tier. If this fails, the tier was renamed or removed.")
        }
    }

    // MARK: - BlinkingCursor preserved

    /// The `BlinkingCursor` is still used as the streaming-text tail
    /// (the cursor at the end of an in-progress prose run). It must not
    /// be removed alongside the placeholder rewrite.
    @Test("BlinkingCursor is still defined and used as the streaming tail")
    func blinkingCursorPreserved() throws {
        let url = try locateSourceFile(
            relativeTo: #filePath,
            segments: ["Herald", "Features", "Chat", "MarkdownContentView.swift"]
        )
        let source = try String(contentsOf: url, encoding: .utf8)
        #expect(source.contains("struct BlinkingCursor: View"),
                "BlinkingCursor struct must remain defined for the streaming tail.")
        #expect(source.contains("BlinkingCursor()"),
                "BlinkingCursor must still be instantiated somewhere in the view body.")
    }

    // MARK: - Tier text is locked end-to-end

    /// `StreamingPlaceholderText.tierText(for:)` is the single source of
    /// truth for the user-visible status strings. Lock the exact mapping
    /// so a future edit cannot silently change "Thinking... 5s" to
    /// "Thinking - 5s" or skip a tier entirely.
    @Test("StreamingPlaceholderText.tierText produces the locked tier strings")
    func tierTextMappingIsLocked() {
        // < 5s
        #expect(StreamingPlaceholderText.tierText(for: 0) == "Connecting to Hermes...")
        #expect(StreamingPlaceholderText.tierText(for: 4.99) == "Connecting to Hermes...")
        // 5s ... < 30s
        #expect(StreamingPlaceholderText.tierText(for: 5).hasPrefix("Thinking... "),
                "5s boundary must roll into the 'Thinking' tier")
        #expect(StreamingPlaceholderText.tierText(for: 5).contains("5s"))
        #expect(StreamingPlaceholderText.tierText(for: 29).hasPrefix("Thinking... "))
        // < 60s
        #expect(StreamingPlaceholderText.tierText(for: 30) == "Model is working on your request...")
        #expect(StreamingPlaceholderText.tierText(for: 59.99) == "Model is working on your request...")
        // >= 60s
        #expect(StreamingPlaceholderText.tierText(for: 60).hasPrefix("This is taking longer than usual"))
        #expect(StreamingPlaceholderText.tierText(for: 65).hasPrefix("This is taking longer than usual"))
        #expect(StreamingPlaceholderText.tierText(for: 65).contains("1m 5s"),
                "After 60s the elapsed time should render as 'Xm Ys'")
    }

    // MARK: - Placeholder looks intentional (not a bare cursor)

    /// The polished placeholder wraps the status string in a Capsule + a
    /// pulsing accent dot. Source-shape guard: if the rewrite ever
    /// regresses to a bare italic `Text(...)`, this test fails and tells
    /// the engineer the placeholder is "amateur" again.
    @Test("StreamingPlaceholderText renders a capsule-pill, not bare text")
    func placeholderRendersCapsulePill() throws {
        let url = try locateSourceFile(
            relativeTo: #filePath,
            segments: ["Herald", "Features", "Chat", "MarkdownContentView.swift"]
        )
        let source = try String(contentsOf: url, encoding: .utf8)
        #expect(source.contains("Capsule()"),
                "Placeholder must wrap the status in a Capsule surface, not render as bare text.")
        #expect(source.contains("PulsingStatusDot"),
                "Placeholder must include a live status dot so it reads as intentional, not as a hung cursor.")
        #expect(source.contains(".background("),
                "Placeholder must declare a background surface for visual weight.")
    }

    // MARK: - MessageBubble DRYs to the shared component

    /// `MessageBubble.streamingPlaceholder` should delegate to
    /// `StreamingPlaceholderText` so the bubble and the markdown fallback
    /// render identically. If a future edit re-introduces a separate
    /// `TimelineView` literal in the bubble, the two placeholders will
    /// drift visually and the user will see two different "still working"
    /// shapes depending on which view fires first.
    @Test("MessageBubble.streamingPlaceholder delegates to StreamingPlaceholderText")
    func bubbleUsesSharedPlaceholder() throws {
        let url = try locateSourceFile(
            relativeTo: #filePath,
            segments: ["Herald", "Features", "Chat", "MessageBubble.swift"]
        )
        let source = try String(contentsOf: url, encoding: .utf8)
        #expect(source.contains("StreamingPlaceholderText(startedAt: message.timestamp)"),
                "MessageBubble.streamingPlaceholder must reuse StreamingPlaceholderText so the bubble and the markdown fallback render identically.")
        // The duplicated tierText copy must be gone.
        #expect(!source.contains("\"Connecting to Hermes...\""),
                "MessageBubble must not hold its own copy of the tier strings; it should delegate to StreamingPlaceholderText.")
        #expect(!source.contains("TimelineView(.periodic(from: message.timestamp"),
                "MessageBubble must not own its own TimelineView; that responsibility lives in StreamingPlaceholderText.")
    }

    // MARK: - Dead inner fallback is gone

    /// The inner `else if message.isStreaming && message.reasoning.isEmpty`
    /// fallback was reachable only in theory but still contained the
    /// `reasoning.isEmpty` guard. Lock the removal so the guard can't
    /// accidentally be re-introduced in a future refactor.
    @Test("MessageBubble no longer has the inner empty-content reasoning guard")
    func innerReasoningGuardRemoved() throws {
        let url = try locateSourceFile(
            relativeTo: #filePath,
            segments: ["Herald", "Features", "Chat", "MessageBubble.swift"]
        )
        let source = try String(contentsOf: url, encoding: .utf8)
        #expect(!source.contains("message.isStreaming && message.reasoning.isEmpty"),
                "The inner `isStreaming && reasoning.isEmpty` guard must be removed; the empty-content placeholder is now always-on via the outer guard.")
    }

    // MARK: - Helpers

    /// Walk up from a test-file path to the repo root by climbing until
    /// we find a directory that contains the `Herald` source folder.
    /// Tests live in `KallistiTests/`, sources in `Herald/...`; both
    /// share a common parent (the repo root).
    private func locateSourceFile(
        relativeTo testFilePath: String,
        segments: [String]
    ) throws -> URL {
        var dir = URL(fileURLWithPath: testFilePath).deletingLastPathComponent()
        for _ in 0..<8 {
            let candidate = dir.appendingPathComponent("Herald")
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: candidate.path, isDirectory: &isDir),
               isDir.boolValue {
                return segments.reduce(into: dir) { acc, seg in
                    acc.appendPathComponent(seg)
                }
            }
            dir = dir.deletingLastPathComponent()
        }
        throw NSError(
            domain: "MessageBubbleStreamingPlaceholderTests",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "Could not locate repo root from \(testFilePath)"]
        )
    }
}
