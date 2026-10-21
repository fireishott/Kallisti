import SwiftUI

// MARK: - Eris chat
//
// One daemon, one conversation. Answers render as markdown; reasoning renders
// collapsed and de-emphasised; tool output renders as a compact run log. The
// composer sends through `ErisChatViewModel`, which awaits the `ErisClient`
// actor, so nothing here blocks the main actor.

/// The chat surface bound to the paired Eris daemon.
struct ErisChatScreen: View {
    let store: ErisDaemonStore
    let model: ErisChatViewModel
    /// Whether the Forget control is offered.
    ///
    /// The free-standing chat tab offers it. The conversation detail screen does
    /// not: it has its own back control, and unpairing the daemon from inside a
    /// transcript would be a trap rather than a convenience.
    var showsForget: Bool = true

    @State private var draft = ""
    @FocusState private var isComposerFocused: Bool

    private static let bottomAnchor = "eris.chat.bottom"

    var body: some View {
        VStack(spacing: 0) {
            transcript
            composer
        }
        .background(Design.Colors.background.ignoresSafeArea())
        .toolbar {
            if showsForget {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Forget", role: .destructive) {
                        Task { await store.forget() }
                    }
                    .font(Design.Typography.footnote)
                    .accessibilityIdentifier("eris.forgetFromChat")
                }
            }
        }
    }

    // MARK: Transcript

    private var transcript: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Design.Spacing.sm) {
                    daemonLine
                    if model.items.isEmpty {
                        emptyState
                    }
                    ForEach(model.items) { item in
                        row(for: item)
                    }
                    if let banner = model.banner {
                        bannerView(banner)
                    }
                    if let usage = model.usageLine {
                        Text(usage)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                            .padding(.horizontal, Design.Spacing.md)
                    }
                    Color.clear
                        .frame(height: 1)
                        .id(Self.bottomAnchor)
                }
                .padding(.vertical, Design.Spacing.md)
            }
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: model.items.count) { _, _ in
                scrollToBottom(proxy, animated: true)
            }
            .onChange(of: model.items.last?.text) { _, _ in
                scrollToBottom(proxy, animated: false)
            }
        }
    }

    private var daemonLine: some View {
        HStack(spacing: Design.Spacing.xxs) {
            Circle()
                .fill(Design.Colors.success)
                .frame(width: 6, height: 6)
            Text(store.pairedDisplayAddress ?? "paired daemon")
                .font(Design.Typography.caption2)
                .foregroundStyle(Design.Colors.tertiaryForeground)
            if store.isVerifying {
                Text("· checking")
                    .font(Design.Typography.caption2)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
            Spacer()
        }
        .padding(.horizontal, Design.Spacing.md)
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text(model.boundConversationID == nil ? "Eris is paired" : "Nothing said here yet")
                .font(Design.Typography.sectionTitle)
                .foregroundStyle(Design.Colors.foreground)
            Text(
                model.boundConversationID == nil
                    ? "Send a message to start the first turn."
                    : "Send a message and it is filed under this conversation."
            )
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Rows

    @ViewBuilder
    private func row(for item: ErisChatItem) -> some View {
        switch item.kind {
        case .question:
            questionRow(item)
        case .answer:
            answerRow(item)
        case .reasoning:
            reasoningRow(item)
        case .toolRun:
            toolRow(item)
        case .failure:
            failureRow(item)
        case .notice:
            Text(item.text)
                .font(Design.Typography.caption2)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.horizontal, Design.Spacing.md)
        }
    }

    private func questionRow(_ item: ErisChatItem) -> some View {
        HStack {
            Spacer(minLength: Design.Spacing.xl)
            Text(item.text)
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.background)
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, Design.Spacing.xs)
                .background(Design.Brand.accent)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        }
        .padding(.horizontal, Design.Spacing.md)
    }

    private func answerRow(_ item: ErisChatItem) -> some View {
        MarkdownContentView(content: item.text, isStreaming: item.isStreaming)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Design.Spacing.md)
    }

    /// Reasoning: collapsed by default, quiet, monospaced.
    private func reasoningRow(_ item: ErisChatItem) -> some View {
        DisclosureGroup {
            Text(item.text.trimmingCharacters(in: .whitespacesAndNewlines))
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(Design.Spacing.sm)
                .background(Design.Colors.surface)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
                .padding(.top, Design.Spacing.xxs)
        } label: {
            HStack(spacing: Design.Spacing.xxs) {
                Image(systemName: "brain")
                    .font(.system(size: Design.Size.iconTiny, weight: .semibold))
                Text(item.isStreaming ? "Reasoning…" : "Reasoning")
                    .font(Design.Typography.caption2)
                Spacer()
            }
            .foregroundStyle(Design.Colors.tertiaryForeground)
        }
        .padding(.horizontal, Design.Spacing.md)
        .accessibilityIdentifier("eris.reasoning")
    }

    /// Tool output: a compact run log, one prefixed line per action.
    private func toolRow(_ item: ErisChatItem) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
            ForEach(Array(logLines(item.text).enumerated()), id: \.offset) { _, line in
                HStack(alignment: .firstTextBaseline, spacing: Design.Spacing.xxs) {
                    Text("›")
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.accent)
                    Text(line)
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.xs)
        .background(Design.Colors.surface2)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
        .padding(.horizontal, Design.Spacing.md)
        .accessibilityIdentifier("eris.toolRun")
    }

    private func failureRow(_ item: ErisChatItem) -> some View {
        Text(item.text)
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.danger)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Design.Spacing.sm)
            .background(Design.Colors.surface)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            .padding(.horizontal, Design.Spacing.md)
    }

    private func bannerView(_ text: String) -> some View {
        Text(text)
            .font(Design.Typography.helper)
            .foregroundStyle(Design.Colors.warning)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Design.Spacing.md)
    }

    // MARK: Composer

    private var composer: some View {
        VStack(spacing: 0) {
            Divider().overlay(Design.Colors.divider)
            HStack(alignment: .bottom, spacing: Design.Spacing.xs) {
                TextField("Message Eris", text: $draft, axis: .vertical)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .lineLimit(1...6)
                    .focused($isComposerFocused)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.vertical, Design.Spacing.xs)
                    .background(Design.Colors.surface)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                    .accessibilityIdentifier("eris.composer")

                if model.isStreaming {
                    Button {
                        Task { await model.stop() }
                    } label: {
                        Text(model.isStopping ? "Stopping…" : "Stop")
                            .font(Design.Typography.footnote)
                            .foregroundStyle(Design.Colors.foreground)
                            .padding(.horizontal, Design.Spacing.sm)
                            .frame(minHeight: Design.Size.minTapTarget)
                            .background(Design.Colors.surface2)
                            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.pill))
                    }
                    .buttonStyle(.plain)
                    .disabled(model.isStopping)
                    .accessibilityIdentifier("eris.stop")
                } else {
                    Button(action: send) {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: Design.Size.iconMedium))
                            .foregroundStyle(
                                model.canSend(draft)
                                    ? Design.Brand.accent
                                    : Design.Colors.tertiaryForeground
                            )
                            .frame(minWidth: Design.Size.minTapTarget, minHeight: Design.Size.minTapTarget)
                    }
                    .buttonStyle(.plain)
                    .disabled(!model.canSend(draft))
                    .accessibilityIdentifier("eris.send")
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.xs)
        }
        .background(Design.Colors.background)
    }

    // MARK: Actions

    private func send() {
        guard model.canSend(draft) else { return }
        let text = draft
        draft = ""
        Task { await model.send(text) }
    }

    // MARK: Helpers

    /// Drop blank lines so the run log stays compact.
    private func logLines(_ text: String) -> [String] {
        text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool) {
        if animated {
            withAnimation(Design.Motion.quickResponse) {
                proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
            }
        } else {
            proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
        }
    }
}
