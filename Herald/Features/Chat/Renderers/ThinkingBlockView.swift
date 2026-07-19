import SwiftUI

/// Inline renderer for `<think>…</think>` blocks parsed from message content.
/// Collapsed by default; tap header to expand. While streaming, stays open
/// with a pulsing indicator.
struct ThinkingBlockView: View {
    let content: String
    let isStreaming: Bool

    @State private var isExpanded = false
    @State private var pulse = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header row
            Button {
                withAnimation(.easeInOut(duration: 0.2)) {
                    isExpanded.toggle()
                }
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    if isStreaming && !isExpanded {
                        Circle()
                            .fill(Design.Brand.accent)
                            .frame(width: 6, height: 6)
                            .scaleEffect(pulse ? 1.3 : 0.8)
                            .animation(.easeInOut(duration: 0.6).repeatForever(), value: pulse)
                            .onAppear { pulse = true }
                    } else {
                        Image(systemName: "brain")
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    Text(isStreaming && !isExpanded ? "Thinking…" : "Reasoning")
                        .font(.system(.caption, weight: .medium))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Spacer()
                    Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, Design.Spacing.xs)
            }
            .buttonStyle(.plain)

            // Expandable body
            if isExpanded || (isStreaming && !isExpanded) {
                Text(content)
                    .font(.system(.footnote, design: .default))
                    .italic()
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.85))
                    .textSelection(.enabled)
                    .padding(.horizontal, Design.Spacing.sm)
                    .padding(.bottom, Design.Spacing.sm)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .background(Design.Colors.surface.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
    }
}
