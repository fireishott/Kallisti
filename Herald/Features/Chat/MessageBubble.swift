import SwiftUI
import UIKit

struct MessageBubble: View, Equatable {
    let message: Message
    var onRetry: ((Message) -> Void)? = nil
    var onDelete: ((Message) -> Void)? = nil
    var onOpenCanvas: ((Message) -> Void)? = nil

    /// Only the message itself affects the rendered bubble — the retry closure
    /// is captured fresh per parent render but is functionally stable. Comparing
    /// messages lets `.equatable()` in the list skip re-rendering unchanged
    /// bubbles while the streaming tail appends.
    nonisolated static func == (lhs: MessageBubble, rhs: MessageBubble) -> Bool {
        lhs.message == rhs.message
    }

    private var isUser: Bool { message.sender == .user || message.sender == .voiceUser }
    private var isHermes: Bool { message.sender == .hermes || message.sender == .voiceHermes }
    private var isCompactionMessage: Bool { message.content.hasPrefix("[CONTEXT COMPACTION]") }
    private var isBudgetWarning: Bool { message.content.contains("[BUDGET WARNING:") }

    var body: some View {
        contentView
            .contextMenu {
                // Copy text — always
                Button {
                    UIPasteboard.general.string = message.content
                } label: {
                    Label("Copy Text", systemImage: "doc.on.doc")
                }

                // Copy first code block — only if message contains one
                let segments = parseMarkdownSegments(message.content)
                if let codeBlock = segments.first(where: {
                    if case .codeBlock = $0 { return true }
                    return false
                }), case .codeBlock(_, _, let code) = codeBlock {
                    Button {
                        UIPasteboard.general.string = code
                    } label: {
                        Label("Copy Code", systemImage: "chevron.left.forwardslash.chevron.right")
                    }
                }

                // Open in Canvas — only if extractable content exists
                let hasCanvas = segments.contains(where: {
                    if case .codeBlock = $0 { return true }
                    return false
                })
                if hasCanvas {
                    Button {
                        onOpenCanvas?(message)
                    } label: {
                        Label("Open in Canvas", systemImage: "rectangle.on.rectangle")
                    }
                }

                Divider()

                // Retry — assistant messages only
                if isHermes {
                    Button {
                        onRetry?(message)
                    } label: {
                        Label("Retry", systemImage: "arrow.counterclockwise")
                    }
                }

                // Share
                Button {
                    let av = UIActivityViewController(activityItems: [message.content], applicationActivities: nil)
                    if let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
                       let root = scene.windows.first?.rootViewController {
                        root.present(av, animated: true)
                    }
                } label: {
                    Label("Share", systemImage: "square.and.arrow.up")
                }

                Divider()

                // Delete — destructive
                Button(role: .destructive) {
                    onDelete?(message)
                } label: {
                    Label("Delete", systemImage: "trash")
                }
            }
    }

    @ViewBuilder
    private var contentView: some View {
        if message.sender == .system && message.content.contains("[Voice session ended]") {
            VoiceSessionBanner(duration: message.voiceSessionDuration)
        } else if message.sender == .system {
            systemMessage
        } else if isCompactionMessage {
            compactionBanner
        } else if isUser {
            HStack(alignment: .top, spacing: Design.Spacing.xs) {
                Spacer(minLength: Design.Spacing.xxl)
                userBubble
            }
            .padding(.horizontal, Design.Spacing.md)
        } else {
            HStack(alignment: .top, spacing: Design.Spacing.xs) {
                hermesMessage
                Spacer(minLength: Design.Spacing.xxl)
            }
            .padding(.horizontal, Design.Spacing.md)
        }
    }

    // MARK: - System Message

    private var systemMessage: some View {
        Text(message.content)
            .brandEyebrow()
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, Design.Spacing.lg)
            .padding(.vertical, Design.Spacing.xxs)
    }

    // MARK: - User Bubble

    private var userBubble: some View {
        VStack(alignment: .trailing, spacing: Design.Spacing.xxs) {
            if message.isVoiceTranscript {
                voiceTranscriptText(message.content)
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.sm)
                    .background(Design.Colors.surface2)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xxl))

                voiceModeLabel
            } else {
                VStack(alignment: .trailing, spacing: Design.Spacing.xxs) {
                    // Attachment thumbnails
                    if !message.attachments.isEmpty {
                        MessageAttachmentsView(attachments: message.attachments, alignment: .trailing)
                    }

                    // Text content (skip if it's just the auto-generated attachment placeholder)
                    let isAttachmentPlaceholder = !message.attachments.isEmpty
                        && message.content.range(of: #"^\[\d+ attachment"#, options: .regularExpression) != nil
                    if !message.content.isEmpty && !isAttachmentPlaceholder {
                        MarkdownContentView(content: message.content, isStreaming: false)
                            .foregroundStyle(Design.Colors.foreground)
                            .padding(.horizontal, Design.Spacing.md)
                            .padding(.vertical, Design.Spacing.sm)
                            .background(Design.Colors.surface2)
                            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xxl))
                    }
                }

                HStack(spacing: Design.Spacing.xs) {
                    Text(message.timestamp, style: .time)
                        .brandEyebrow()

                    Image(systemName: message.status.displayIcon)
                        .font(.system(size: Design.Size.iconTiny))
                        .foregroundStyle(message.status.displayColor)
                        .accessibilityLabel(message.status.rawValue)
                }
            }

            if message.status == .failed {
                Button { onRetry?(message) } label: {
                    Label("Retry", systemImage: "arrow.clockwise")
                        .brandEyebrow(Design.Colors.danger)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(message.isVoiceTranscript ? "Voice" : "You"): \(message.content). \(message.status.rawValue)")
    }

    // MARK: - Herald Message

    private var hermesMessage: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
            if message.isVoiceTranscript {
                voiceTranscriptText(message.content)
                    .padding(.vertical, Design.Spacing.xxs)

                voiceModeLabel
            } else if message.isStreaming && message.content.isEmpty && message.toolActivities.isEmpty && message.reasoning.isEmpty {
                streamingPlaceholder
            } else {
                if !message.reasoning.isEmpty {
                    ReasoningView(
                        reasoning: message.reasoning,
                        isStreaming: message.isStreaming && message.content.isEmpty,
                        duration: message.reasoningDuration
                    )
                }

                if !message.content.isEmpty {
                    streamingText
                } else if message.isStreaming && message.reasoning.isEmpty {
                    // Content still empty but tool activities exist — show a subtle placeholder
                    streamingPlaceholder
                }

                if !message.toolActivities.isEmpty {
                    ToolActivityRail(
                        activities: message.toolActivities,
                        isStreaming: message.isStreaming
                    )
                } else if let activity = message.toolActivity {
                    toolActivityPill(activity)
                }

                if let diff = message.codeDiff, !diff.isEmpty {
                    InlineDiffView(diff: diff)
                }

                if !message.attachments.isEmpty {
                    MessageAttachmentsView(attachments: message.attachments, alignment: .leading)
                }

                if !message.isStreaming {
                    Text(message.timestamp, style: .time)
                        .brandEyebrow()
                }

                if message.status == .failed {
                    Button { onRetry?(message) } label: {
                        Label("Regenerate", systemImage: "arrow.counterclockwise")
                            .brandEyebrow(Design.Brand.accent)
                    }
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Herald: \(message.content)")
        .accessibilityAddTraits(message.isStreaming ? .updatesFrequently : [])
    }

    // MARK: - Voice Transcript Components

    private func voiceTranscriptText(_ content: String) -> some View {
        Text("\u{201C}\(content)\u{201D}")
            .font(Design.Typography.editorialItalicSmall)
            .foregroundStyle(Design.Colors.foreground.opacity(0.88))
    }

    private var voiceModeLabel: some View {
        Text("Voice Mode")
            .brandEyebrow(Design.Colors.tertiaryForeground)
    }

    // MARK: - Streaming Components

    @ViewBuilder
    private var streamingText: some View {
        let displayContent = isBudgetWarning
            ? Self.strippingBudgetWarnings(from: message.content)
            : message.content

        MarkdownContentView(
            content: displayContent,
            isStreaming: message.isStreaming,
            showCursor: message.isStreaming
        )
        .foregroundStyle(Design.Colors.foreground)
        .padding(.vertical, Design.Spacing.xxs)
    }

    private var streamingPlaceholder: some View {
        TypingDotsView()
            .padding(.vertical, Design.Spacing.sm)
    }

    private func toolActivityPill(_ label: String) -> some View {
        Text(label)
            .brandEyebrow()
            .padding(.horizontal, Design.Spacing.sm)
            .padding(.vertical, Design.Spacing.xxs)
            .background(Design.Colors.surface)
            .overlay(
                Capsule().stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(Capsule())
    }

    // MARK: - Context Compaction Banner

    private var compactionBanner: some View {
        HStack(spacing: Design.Spacing.xs) {
            Rectangle()
                .fill(Design.Colors.border)
                .frame(height: 1)
            Text("Context compacted")
                .brandEyebrow()
                .fixedSize()
            Rectangle()
                .fill(Design.Colors.border)
                .frame(height: 1)
        }
        .padding(.horizontal, Design.Spacing.lg)
        .padding(.vertical, Design.Spacing.sm)
    }

    // MARK: - Budget Warning Stripping

    /// Strips `[BUDGET WARNING: ...]` lines injected by the Herald agent into
    /// tool result messages.  These are internal agent housekeeping and should
    /// not be shown to the user verbatim.
    static func strippingBudgetWarnings(from text: String) -> String {
        text.replacingOccurrences(
            of: #"\[BUDGET WARNING:[^\]]*\]"#,
            with: "",
            options: .regularExpression
        ).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
