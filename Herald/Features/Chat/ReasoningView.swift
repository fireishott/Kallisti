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
    @State private var pulseOpacity: Double = 1.0

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
        // Use task(id:) so animation state is always re-synchronized on
        // isStreaming value changes AND on any view re-mount (LazyVStack
        // recycle, etc.). onAppear+onChange missed the re-mount case where
        // the view is removed and re-inserted while isStreaming is already
        // true — leaving a non-pulsing, collapsed header mid-stream.
        .task(id: isStreaming) {
            if isStreaming {
                isExpanded = true
                withAnimation(
                    .easeInOut(duration: 0.9)
                    .repeatForever(autoreverses: true)
                ) {
                    pulseOpacity = 0.35
                }
            } else {
                withAnimation(Design.Motion.standard) {
                    isExpanded = false
                    pulseOpacity = 1.0
                }
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
                    .opacity(isStreaming ? pulseOpacity : 1.0)

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
