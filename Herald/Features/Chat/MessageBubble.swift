import SwiftUI
import UIKit

struct MessageBubble: View, Equatable {
    let message: Message
    /// User-chosen chat text color hex, threaded from the parent so the
    /// `.equatable()` optimization can detect a color change (the message
    /// itself is unchanged when the picker writes a new color).
    let textColorHex: String?
    var onRetry: ((Message) -> Void)? = nil
    var onStartNewSession: (() -> Void)? = nil
    @Environment(TalkStore.self) private var talkStore
    @Environment(SettingsStore.self) private var settingsStore
    var onDelete: ((Message) -> Void)? = nil
    var onOpenCanvas: ((Message) -> Void)? = nil
    @State private var showReactionPicker = false
    @State private var reactions: [String] = []

    /// Only the message itself affects the rendered bubble — the retry closure
    /// is captured fresh per parent render but is functionally stable. Comparing
    /// messages lets `.equatable()` in the list skip re-rendering unchanged
    /// bubbles while the streaming tail appends.
    nonisolated static func == (lhs: MessageBubble, rhs: MessageBubble) -> Bool {
        // Always re-render streaming messages — the equatable optimization
        // must not skip the active streaming tail, whose content mutates
        // at 30fps via delta coalescing.
        guard !lhs.message.isStreaming && !rhs.message.isStreaming else { return false }
        // The chat text color lives in Settings, not the message, so a color
        // change must re-render even when the message itself is unchanged.
        guard lhs.textColorHex == rhs.textColorHex else { return false }
        return lhs.message == rhs.message
    }

    private var isUser: Bool { message.sender == .user || message.sender == .voiceUser }
    private var isHermes: Bool { message.sender == .herald || message.sender == .voiceHerald }
    private var isCompactionMessage: Bool { message.content.hasPrefix("[CONTEXT COMPACTION]") }
    private var isBudgetWarning: Bool { message.content.contains("[BUDGET WARNING:") }

    /// Build 86: user-chosen chat text color from Appearance settings.
    /// Falls back to the theme foreground when unset or unparseable.
    private var chatTextColor: Color {
        guard let hex = textColorHex,
              !hex.isEmpty
        else { return Design.Colors.foreground }
        let cleaned = hex.trimmingCharacters(in: CharacterSet(charactersIn: "# "))
        guard let value = UInt(cleaned, radix: 16), cleaned.count <= 6 else {
            return Design.Colors.foreground
        }
        return Color(hex: value)
    }

    /// Strips all Hermes inline-reference directives the desktop gateway
    /// persists alongside user-attached files/images/links/etc. The
    /// attachments themselves render from `attachments[]`; the directive
    /// text must never leak into the bubble.
    ///
    /// Mirrors the wire grammar shared by the desktop renderer
    /// (`reference-kinds.ts`) and the Python gateway
    /// (`agent/context_references.py`):
    ///   - valued kinds: `file | folder | git | url | image | tool | line | terminal | session`
    ///     followed by a value (backtick/double/single-quoted or bare `\S+`),
    ///     with an optional trailing `:N` or `:N-M` line-range and an
    ///     optional `{name=...}` suffix.
    ///   - valueless refs: bare `@diff` and `@staged`.
    ///   - canonical bracket form: `@image:[alt]({name=...})` (also stripped).
    ///   - does NOT strip ordinary `@mentions` (e.g. `@user`).
    private func stripImageDirectives(_ content: String) -> String {
        // One regex covering every recognised Hermes inline-directive shape,
        // matched anywhere in the string. The lookbehind prevents stripping
        // ordinary mentions like @user / @team-lead. Raw string (#"...") so
        // regex backslashes and quotes need no escaping.
        let pattern = #"(?<![\w/])@(?:(?:diff|staged)\b|image:\[[^\]\n]*\](?:\(\{name=[^}\n]*\}\))?|(?:file|folder|git|url|image|tool|line|terminal|session):(`[^`\n]+`|"[^"\n]+"|'[^'\n]+'|\S+)(?::\d+(?:-\d+)?(?=$|[\s,;\]\}"'\)`]))?(?:\{name=[^}\n]*\})?)"#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.dotMatchesLineSeparators]) else {
            return content.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        let range = NSRange(content.startIndex..<content.endIndex, in: content)
        let stripped = regex.stringByReplacingMatches(
            in: content,
            options: [],
            range: range,
            withTemplate: ""
        )
        return stripped.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        contentView
            .sheet(isPresented: $showReactionPicker) {
                MessageReactionPicker { reaction in
                    addReaction(reaction, to: message)
                }
                .presentationDetents([.height(120)])
                .presentationDragIndicator(.hidden)
            }
    }

    // MARK: - Actions Menu

    /// "..." menu that replaces the old long-press `.contextMenu`. The context
    /// menu was stealing the long-press gesture, so text selection never fired
    /// and the only copy path was "Copy Text" (whole message). Moving the
    /// actions to an explicit button frees the long-press for
    /// `.textSelection(.enabled)`, which gives partial-text selection plus the
    /// system "Copy" for the selected range.
    private var bubbleMenu: some View {
        Menu {
            Button {
                UIPasteboard.general.string = message.content
            } label: {
                Label("Copy Text", systemImage: "doc.on.doc")
            }

            // Copy first code block - only if message contains one
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

            // Open in Canvas - only if extractable content exists
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

            // React
            Button {
                showReactionPicker = true
            } label: {
                Label("React", systemImage: "face.smiling")
            }

            Divider()

            // Retry - show for EVERY message so the action is always
            // discoverable. Build 127: was gated on `isHermes`, which hid it
            // on messages whose sender was not flagged herald/voiceHerald
            // (e.g. streamed rows, tool-boundary rows, errored replies) —
            // Curtis: "also the retry button", the menu was inconsistent
            // between two screenshots of the same thread. Retrying a user
            // message resends it; retrying an assistant message re-asks.
            Button {
                onRetry?(message)
            } label: {
                Label("Retry", systemImage: "arrow.counterclockwise")
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

            // Delete - destructive
            Button(role: .destructive) {
                onDelete?(message)
            } label: {
                Label("Delete", systemImage: "trash")
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: Design.Size.iconTiny, weight: .medium))
                .foregroundStyle(Design.Colors.secondaryForeground)
                .frame(width: 26, height: 26)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
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
        VStack(spacing: Design.Spacing.xs) {
            Text(message.content)
                .brandEyebrow()
                .multilineTextAlignment(.center)

            if message.status == .failed, let category = message.errorCategory {
                errorGuidanceChip(for: category)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, Design.Spacing.lg)
        .padding(.vertical, Design.Spacing.xxs)
    }

    @ViewBuilder
    private func errorGuidanceChip(for category: String) -> some View {
        switch category {
        case "context_exceeded":
            Button { onStartNewSession?() } label: {
                Label("Start New Session", systemImage: "plus.message")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Brand.accent)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xxs)
                    .background(Capsule().fill(Design.Brand.accent.opacity(0.12)))
            }
            .buttonStyle(.plain)
        case "rate_limited":
            Button { onRetry?(message) } label: {
                Label("Wait & Retry", systemImage: "clock.arrow.circlepath")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xxs)
                    .background(Capsule().fill(Design.Colors.warning.opacity(0.12)))
            }
            .buttonStyle(.plain)
        case "timeout":
            Button { onRetry?(message) } label: {
                Label("Retry", systemImage: "arrow.clockwise")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Brand.accent)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xxs)
                    .background(Capsule().fill(Design.Brand.accent.opacity(0.12)))
            }
            .buttonStyle(.plain)
        case "empty_response":
            Button { onRetry?(message) } label: {
                Label("Retry", systemImage: "arrow.clockwise")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xxs)
                    .background(Capsule().fill(Design.Colors.warning.opacity(0.12)))
            }
            .buttonStyle(.plain)
        case "session_busy":
            // Build 132: this conversation is mid-turn on another device.
            // Retrying into the same busy session just re-fails - offer the
            // user a fresh session instead.
            Button { onStartNewSession?() } label: {
                Label("Start New Session", systemImage: "plus.message")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Brand.accent)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xxs)
                    .background(Capsule().fill(Design.Brand.accent.opacity(0.12)))
            }
            .buttonStyle(.plain)
        default:
            Button { onRetry?(message) } label: {
                Label("Retry", systemImage: "arrow.clockwise")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xxs)
                    .background(Capsule().fill(Design.Colors.surface))
            }
            .buttonStyle(.plain)
        }
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
                        MarkdownContentView(content: stripImageDirectives(message.content), isStreaming: false, textColor: chatTextColor)
                            .foregroundStyle(chatTextColor)
                            .textSelection(.enabled)
                            .padding(.horizontal, Design.Spacing.md)
                            .padding(.vertical, Design.Spacing.sm)
                            .background(Design.Colors.surface2)
                            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xxl))
                    }
                }

                HStack(spacing: Design.Spacing.xs) {
                    if message.isStreaming {
                        TimelineView(.periodic(from: .now, by: 1)) { context in
                            Text(context.date, style: .time)
                                .brandEyebrow()
                        }
                    } else {
                        Text(message.timestamp, style: .time)
                            .brandEyebrow()
                    }

                    Image(systemName: message.status.displayIcon)
                        .font(.system(size: Design.Size.iconTiny))
                        .foregroundStyle(message.status.displayColor)
                        .accessibilityLabel(message.status.rawValue)

                    bubbleMenu
                }
            }

            if message.status == .failed {
                Button { onRetry?(message) } label: {
                    Label("Retry", systemImage: "arrow.clockwise")
                        .brandEyebrow(Design.Colors.danger)
                }
            }

            reactionBadge
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
            } else if message.isStreaming && message.content.isEmpty && message.reasoning.isEmpty && message.toolActivities.isEmpty {
                // Before the first visible prose token, show the honest status pill
                // only while no reasoning is available AND no tool is in flight.
                // Once reasoning deltas arrive — or a tool call starts — fall
                // through so the live thought tail (ReasoningView) and the tool
                // rail (ToolActivityRail) render instead of being hidden behind
                // a generic spinner.
                streamingPlaceholder
            } else {
                if !message.reasoning.isEmpty && settingsStore.settings.showReasoning {
                    ReasoningView(
                        reasoning: message.reasoning,
                        isStreaming: message.isStreaming,
                        duration: message.reasoningDuration
                    )
                    .transition(.opacity)
                }

                if !message.content.isEmpty {
                    streamingText
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

                // Build 128.54: a streaming row must never show a frozen
                // creation stamp (7:59 PM for 40 minutes while the turn runs).
                // While streaming, render a LIVE clock via TimelineView so the
                // stamp ticks with the current time; once settled, show the
                // completion time stamped by the .finished handler.
                if message.isStreaming {
                    HStack(spacing: Design.Spacing.xs) {
                        TimelineView(.periodic(from: .now, by: 1)) { context in
                            Text(context.date, style: .time)
                                .brandEyebrow()
                        }
                        bubbleMenu
                    }
                } else {
                    HStack(spacing: Design.Spacing.xs) {
                        Text(message.timestamp, style: .time)
                            .brandEyebrow()
                        bubbleMenu
                    }
                }

                if message.status == .failed {
                    Button { onRetry?(message) } label: {
                        Label("Regenerate", systemImage: "arrow.counterclockwise")
                            .brandEyebrow(Design.Brand.accent)
                    }
                }

                reactionBadge
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Kallisti: \(message.content)")
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
        let displayContent = stripImageDirectives(
            isBudgetWarning
                ? Self.strippingBudgetWarnings(from: message.content)
                : message.content
        )

        MarkdownContentView(
            content: displayContent,
            isStreaming: message.isStreaming,
            showCursor: message.isStreaming,
            showReasoning: settingsStore.settings.showReasoning,
            hasStreamedReasoning: !message.reasoning.isEmpty,
            startedAt: message.timestamp,
            textColor: chatTextColor
        )
        .foregroundStyle(chatTextColor)
        .textSelection(.enabled)
        .padding(.vertical, Design.Spacing.xxs)
    }

    /// The streaming placeholder for the empty-content streaming case. Uses
    /// the same tiered pill as `StreamingPlaceholderText` (defined in
    /// `MarkdownContentView.swift`) so the bubble and the markdown fallback
    /// render identically. The reasoning behind a single source of truth is
    /// that the user should never see two different shapes for "Hermes is
    /// working" depending on which view happened to render first.
    private var streamingPlaceholder: some View {
        StreamingPlaceholderText(startedAt: message.timestamp)
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

    // MARK: - Reactions

    @ViewBuilder
    private var reactionBadge: some View {
        if !reactions.isEmpty {
            HStack(spacing: 2) {
                ForEach(Array(reactions.enumerated()), id: \.offset) { _, reaction in
                    Text(reaction)
                        .font(.system(size: 14))
                }
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Design.Colors.surface)
            .clipShape(Capsule())
            .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 0.5))
        }
    }

    private func addReaction(_ reaction: String, to message: Message) {
        reactions.append(reaction)
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
