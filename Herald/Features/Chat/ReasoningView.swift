import SwiftUI

/// Displays a Herald message's streamed reasoning / chain-of-thought.
///
/// While the answer is still streaming, the reasoning shows live in dimmed,
/// italic text under a pulsing "Thinking…" header — kept visually quieter than
/// the answer so it reads as process, not product. Once the final answer
/// arrives the block collapses to a single "Thought for Xs" row that the user
/// can tap to re-expand.
struct ReasoningView: View {
    let reasoning: String
    let isStreaming: Bool
    let duration: TimeInterval?

    @State private var isExpanded = false
    @State private var pulse = false

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            header

            if showBody {
                Text(reasoning)
                    .font(.system(.footnote, design: .default))
                    .italic()
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.85))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xs)
        .background(Design.Colors.surface.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(Design.Colors.divider, lineWidth: 1)
        )
        .onAppear {
            // Collapsed by default once complete; auto-open while streaming.
            isExpanded = isStreaming
            if isStreaming {
                withAnimation(Design.Motion.breathe) { pulse = true }
            }
        }
        .onChange(of: isStreaming) { _, streaming in
            // When streaming ends, collapse the block down to its summary.
            withAnimation(Design.Motion.standard) {
                if !streaming { isExpanded = false }
            }
        }
    }

    /// Body is always visible while streaming (can't be collapsed mid-thought);
    /// after completion it follows the user's expand toggle.
    private var showBody: Bool {
        isStreaming || isExpanded
    }

    private var header: some View {
        Button {
            guard !isStreaming else { return }
            withAnimation(Design.Motion.standard) { isExpanded.toggle() }
        } label: {
            HStack(spacing: Design.Spacing.xs) {
                Image(systemName: "brain")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .opacity(isStreaming && pulse ? 0.4 : 1.0)

                Text(headerLabel)
                    .font(.system(.caption, weight: .medium))
                    .foregroundStyle(Design.Colors.secondaryForeground)

                Spacer(minLength: 0)

                if !isStreaming {
                    Image(systemName: "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .rotationEffect(.degrees(isExpanded ? 0 : -90))
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isStreaming)
    }

    private var headerLabel: String {
        if isStreaming {
            return "Thinking…"
        }
        if let duration, duration >= 1 {
            return "Thought for \(Int(duration.rounded()))s"
        }
        return "Thought process"
    }
}
