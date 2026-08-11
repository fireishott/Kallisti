import AVKit
import Photos
import SwiftUI

/// Renders message content with inline markdown formatting, fenced code blocks,
/// inline images, thinking blocks, tool calls, and tables.
/// Images from markdown (`![alt](url)`) are rendered as tappable async-loaded
/// previews that open in a fullscreen viewer.
struct MarkdownContentView: View {
    let content: String
    let isStreaming: Bool
    var showCursor: Bool = false
    var showReasoning: Bool = true
    var hasStreamedReasoning: Bool = false
    var toolActivities: [ToolActivity] = []
    /// Timestamp the bubble started streaming. Drives the tiered status text
    /// rendered when content is empty but we are still streaming (e.g. while
    /// a thinking model produces its first prose token). Defaults to "now"
    /// so existing call sites keep working.
    var startedAt: Date = Date()

    @State private var fullscreenImage: MarkdownSegment?
    @State private var cachedSegments: [MarkdownSegment] = []
    @State private var cachedContent: String = ""
    @State private var cachedStreaming: Bool = false

    /// Parse markdown at most once per content change. SwiftUI re-evaluates
    /// `body` any time an ancestor invalidates; without this cache the parser
    /// (line split + regex scan + AttributedString coercion) runs even when
    /// `content` hasn't changed, which dominated streaming CPU.
    private func currentSegments() -> [MarkdownSegment] {
        if cachedContent == content && cachedStreaming == isStreaming {
            return cachedSegments
        }
        return parseMarkdownSegments(content, isStreaming: isStreaming, toolActivities: toolActivities)
    }

    var body: some View {
        let segments = currentSegments()

        if segments.isEmpty && showCursor {
            StreamingPlaceholderText(startedAt: startedAt)
        } else {
            VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                ForEach(Array(segments.enumerated()), id: \.element.id) { index, segment in
                    switch segment {
                    case .prose(_, let text):
                        proseView(text, isLast: index == segments.count - 1)
                    case .codeBlock(_, let language, let code):
                        CodeBlockView(language: language, code: code)
                    case .image(_, let url, let altText):
                        inlineImageView(url: url, altText: altText, segment: segment)
                    case .video(_, let url, let altText):
                        inlineVideoView(url: url, altText: altText)
                    case .thinking(_, let thinkContent):
                        if showReasoning {
                            ThinkingBlockView(content: thinkContent, isStreaming: isStreaming)
                        }
                    case .toolCall(_, let name, let args, let result):
                        ToolCallBubbleView(name: name, args: args, result: result)
                    case .table(_, let rows):
                        TableBlockView(rows: rows)
                    }
                }
            }
            .fullScreenCover(item: $fullscreenImage) { segment in
                if case .image(_, let url, let altText) = segment {
                    ImageViewerScreen(url: url, altText: altText)
                }
            }
            .onChange(of: content, initial: true) { _, _ in
                updateCache(segments)
            }
            .onChange(of: isStreaming) { _, _ in
                updateCache(segments)
            }
        }
    }

    private func updateCache(_ segments: [MarkdownSegment]) {
        guard cachedContent != content || cachedStreaming != isStreaming else { return }
        cachedSegments = segments
        cachedContent = content
        cachedStreaming = isStreaming
    }

    @ViewBuilder
    private func proseView(_ text: String, isLast: Bool) -> some View {
        if showCursor && isLast {
            HStack(alignment: .lastTextBaseline, spacing: 0) {
                formattedText(text)
                BlinkingCursor()
            }
        } else {
            formattedText(text)
        }
    }

    private func formattedText(_ text: String) -> Text {
        if let attributed = try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) {
            return Text(attributed)
                .font(Design.Typography.body)
                .foregroundColor(Design.Colors.foreground)
        } else {
            return Text(text)
                .font(Design.Typography.body)
                .foregroundColor(Design.Colors.foreground)
        }
    }

    // MARK: - Inline Image

    private func inlineImageView(url: URL, altText: String, segment: MarkdownSegment) -> some View {
        Button {
            fullscreenImage = segment
        } label: {
            AuthenticatedAsyncImage(url: url) { phase in
                switch phase {
                case .success(let image):
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: 260, maxHeight: 200)
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))

                case .failure:
                    HStack(spacing: Design.Spacing.xxs) {
                        Image(systemName: "photo.badge.exclamationmark")
                            .font(.caption)
                        Text(altText.isEmpty ? "Image failed to load" : altText)
                            .font(Design.Typography.caption)
                    }
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .padding(Design.Spacing.sm)
                    .background(Design.Colors.surface)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))

                case .empty:
                    RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                        .fill(Design.Colors.surface)
                        .frame(width: 200, height: 140)
                        .overlay {
                            ProgressView()
                                .tint(Design.Colors.secondaryForeground)
                        }

                @unknown default:
                    EmptyView()
                }
            }
        }
        .buttonStyle(.plain)
    }

    // MARK: - Inline Video

    @ViewBuilder
    private func inlineVideoView(url: URL, altText: String) -> some View {
        let isYouTube = url.host?.contains("youtube") == true
            || url.host?.contains("youtu.be") == true
            || url.host?.contains("vimeo") == true
        if isYouTube {
            Link(destination: url) {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "play.rectangle.fill")
                        .font(.system(size: 28))
                        .foregroundStyle(Design.Brand.accent)
                    VStack(alignment: .leading) {
                        Text(altText.isEmpty ? "Watch Video" : altText)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)
                        Text(url.host ?? "Video")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
                .padding(Design.Spacing.sm)
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                .overlay(RoundedRectangle(cornerRadius: Design.CornerRadius.md).stroke(Design.Colors.border, lineWidth: 1))
            }
            .buttonStyle(.plain)
        } else {
            VideoPlayerView(url: url)
                .frame(maxWidth: 300, maxHeight: 200)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        }
    }
}

// MARK: - Fullscreen Image Viewer

struct ImageViewerScreen: View {
    let url: URL
    let altText: String

    @Environment(\.dismiss) private var dismiss
    @State private var savedToPhotos = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            AsyncImage(url: url) { phase in
                switch phase {
                case .success(let image):
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .ignoresSafeArea()

                case .failure:
                    VStack(spacing: Design.Spacing.md) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.largeTitle)
                            .foregroundStyle(.secondary)
                        Text("Failed to load image")
                            .foregroundStyle(.secondary)
                    }

                case .empty:
                    ProgressView()
                        .tint(.white)
                        .scaleEffect(1.5)

                @unknown default:
                    EmptyView()
                }
            }
        }
        .overlay(alignment: .topTrailing) {
            Button { dismiss() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 16, weight: .bold))
                    .foregroundStyle(.white)
                    .frame(width: 36, height: 36)
                    .background(.ultraThinMaterial)
                    .clipShape(Circle())
            }
            .padding()
        }
        .overlay(alignment: .bottom) {
            HStack(spacing: Design.Spacing.lg) {
                // Download to Photos
                Button {
                    downloadToPhotos()
                } label: {
                    Label(
                        savedToPhotos ? "Saved" : (saveError ?? "Save to Photos"),
                        systemImage: savedToPhotos ? "checkmark.circle.fill" : (saveError != nil ? "exclamationmark.triangle" : "arrow.down.to.line")
                    )
                    .font(Design.Typography.eyebrow)
                    .textCase(.uppercase)
                    .tracking(1.2)
                    .foregroundStyle(saveError != nil ? Design.Colors.danger : .white)
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.sm)
                    .background(.ultraThinMaterial)
                    .clipShape(Capsule())
                }
                .disabled(savedToPhotos)

                // Share
                ShareLink(item: url) {
                    Image(systemName: "square.and.arrow.up")
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(.white)
                        .frame(width: 44, height: 44)
                        .background(.ultraThinMaterial)
                        .clipShape(Circle())
                }
            }
            .padding(.bottom, Design.Spacing.xxl)
        }
        .statusBarHidden(true)
    }

    @State private var saveError: String?

    private func downloadToPhotos() {
        Task {
            do {
                // Check photo library authorization first
                let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
                guard status == .authorized || status == .limited else {
                    withAnimation { saveError = "Photo library access denied" }
                    return
                }

                let (data, _) = try await URLSession.shared.data(from: url)
                guard let uiImage = UIImage(data: data) else {
                    withAnimation { saveError = "Invalid image data" }
                    return
                }

                // Save using PHPhotoLibrary for proper completion handling
                try await PHPhotoLibrary.shared().performChanges {
                    PHAssetChangeRequest.creationRequestForAsset(from: uiImage)
                }
                withAnimation { savedToPhotos = true }
            } catch {
                withAnimation { saveError = "Save failed" }
            }
        }
    }
}

// MARK: - Blinking Cursor

/// An animated text cursor that blinks at the end of streaming content.
struct BlinkingCursor: View {
    @State private var isVisible = true

    var body: some View {
        Text("|")
            .font(Design.Typography.body)
            .foregroundStyle(Design.Brand.accent)
            .opacity(isVisible ? 1 : 0)
            .onAppear {
                withAnimation(.easeInOut(duration: 0.5).repeatForever(autoreverses: true)) {
                    isVisible = false
                }
            }
    }
}

// MARK: - Streaming Placeholder Text

/// Tiered, deliberately-shaped status pill shown while a stream is open but no
/// content has arrived yet. Replaces the bare blinking cursor that used to
/// appear for thinking models whose reasoning/CoT was still streaming while
/// `message.content` was empty.
///
/// Tier thresholds:
/// - < 5s   "Connecting to Hermes..."
/// - < 30s  "Thinking... Xs"
/// - < 60s  "Model is working on your request..."
/// - >=60s  "This is taking longer than usual..."
struct StreamingPlaceholderText: View {
    let startedAt: Date

    var body: some View {
        TimelineView(.periodic(from: startedAt, by: 1)) { context in
            let elapsed = context.date.timeIntervalSince(startedAt)
            let text = Self.tierText(for: elapsed)
            HStack(spacing: Design.Spacing.xs) {
                PulsingStatusDot(tint: Design.Brand.accent)
                Text(text)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
                    .italic()
                    .monospacedDigit()
            }
            .padding(.horizontal, Design.Spacing.sm)
            .padding(.vertical, Design.Spacing.xxs)
            .background(
                Capsule().fill(Design.Colors.surface)
            )
            .overlay(
                Capsule().stroke(Design.Colors.border.opacity(0.6), lineWidth: 0.5)
            )
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Kallisti is generating a response")
    }

    /// The four tiered status strings. Exposed at type scope so callers and
    /// tests can assert on the exact strings. The order is significant:
    /// "Connecting" -> "Thinking" -> "Model is working" -> "longer than usual".
    static func tierText(for elapsed: TimeInterval) -> String {
        if elapsed < 5 {
            return "Connecting to Hermes..."
        } else if elapsed < 30 {
            let seconds = max(1, Int(elapsed))
            return "Thinking... \(seconds)s"
        } else if elapsed < 60 {
            return "Model is working on your request..."
        } else {
            let seconds = Int(elapsed)
            let minutes = seconds / 60
            let rem = seconds % 60
            return rem == 0
                ? "This is taking longer than usual... \(minutes)m"
                : "This is taking longer than usual... \(minutes)m \(rem)s"
        }
    }
}

// MARK: - Pulsing Status Dot

/// Small breathing dot used as the live indicator in `StreamingPlaceholderText`.
/// Pulses in opacity (not size) so it reads as a status heartbeat rather than
/// a spinner. Used as a struct so SwiftUI can render it without a wrapping
/// AnyView (no `[weak self]` concerns).
private struct PulsingStatusDot: View {
    let tint: Color

    @State private var alive: Bool = true

    var body: some View {
        Circle()
            .fill(tint)
            .frame(width: 6, height: 6)
            .opacity(alive ? 1.0 : 0.35)
            .onAppear {
                withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) {
                    alive = false
                }
            }
    }
}
