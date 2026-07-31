import SwiftUI

/// Displays a Herald message's streamed reasoning / chain-of-thought.
///
/// While the answer streams, reasoning shows as a live tail: the newest lines at
/// full weight, older lines fading toward the top of a clipped viewport, under a
/// shimmering "Thinking… Xs" header.  Once the answer arrives it collapses to a
/// tappable "Thought for Xs" row.
struct ReasoningView: View {
    let reasoning: String
    let isStreaming: Bool
    let duration: TimeInterval?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isExpanded = false
    @State private var shimmerPhase: CGFloat = -1
    @State private var startedAt: Date = .now

    private let streamingViewportHeight: CGFloat = 132

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            header
            if showBody { body(for: reasoning) }
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xs)
        .background(Design.Colors.surface.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                .stroke(isStreaming ? Design.Brand.accent.opacity(0.35) : Design.Colors.divider,
                        lineWidth: 1)
        )
        // task(id:) so state re-syncs on value change AND on LazyVStack re-mount.
        .task(id: isStreaming) {
            if isStreaming {
                startedAt = .now
                isExpanded = true
                guard !reduceMotion else { return }
                withAnimation(.linear(duration: 1.6).repeatForever(autoreverses: false)) {
                    shimmerPhase = 2
                }
            } else {
                shimmerPhase = -1
                withAnimation(reduceMotion ? nil : Design.Motion.standard) {
                    isExpanded = false
                }
            }
        }
    }

    // MARK: - Body

    @ViewBuilder
    private func body(for text: String) -> some View {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        let content = VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                Text(line)
                    .font(.system(.footnote, design: .default))
                    .italic()
                    .foregroundStyle(
                        Design.Colors.secondaryForeground.opacity(opacity(for: index, of: lines.count))
                    )
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .id(index)
            }
        }
        .textSelection(.enabled)

        if isStreaming {
            ScrollViewReader { proxy in
                ScrollView(.vertical, showsIndicators: false) {
                    content
                }
                .frame(maxHeight: streamingViewportHeight)
                .mask(
                    LinearGradient(
                        stops: [
                            .init(color: .clear, location: 0.0),
                            .init(color: .black, location: 0.22),
                            .init(color: .black, location: 1.0),
                        ],
                        startPoint: .top, endPoint: .bottom
                    )
                )
                .onChange(of: lines.count) {
                    withAnimation(reduceMotion ? nil : .easeOut(duration: 0.18)) {
                        proxy.scrollTo(max(lines.count - 1, 0), anchor: .bottom)
                    }
                }
            }
            .transition(.opacity)
        } else {
            content.transition(.opacity.combined(with: .move(edge: .top)))
        }
    }

    /// Newest line full strength; each earlier line steps down to a 0.4 floor.
    private func opacity(for index: Int, of total: Int) -> Double {
        guard isStreaming, total > 1 else { return 0.85 }
        let distance = Double(total - 1 - index)
        return max(0.4, 0.95 - distance * 0.11)
    }

    private var showBody: Bool { isStreaming || isExpanded }

    // MARK: - Header

    private var header: some View {
        Button {
            guard !isStreaming else { return }
            withAnimation(reduceMotion ? nil : Design.Motion.standard) { isExpanded.toggle() }
        } label: {
            HStack(spacing: Design.Spacing.xs) {
                Image(systemName: "brain")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(isStreaming ? Design.Brand.accent : Design.Colors.secondaryForeground)

                if isStreaming {
                    TimelineView(.periodic(from: startedAt, by: 1)) { context in
                        shimmering(
                            Text("Thinking… \(Int(context.date.timeIntervalSince(startedAt)))s")
                        )
                    }
                } else {
                    Text(headerLabel)
                        .font(.system(.caption, weight: .medium))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }

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
        .accessibilityLabel(isStreaming ? "Herald is thinking" : headerLabel)
    }

    /// A gradient sweep across the label.  Falls back to flat colour under
    /// Reduce Motion so nothing animates.
    @ViewBuilder
    private func shimmering(_ label: Text) -> some View {
        let styled = label
            .font(.system(.caption, weight: .medium))
            .monospacedDigit()

        if reduceMotion {
            styled.foregroundStyle(Design.Colors.secondaryForeground)
        } else {
            styled
                .foregroundStyle(Design.Colors.secondaryForeground)
                .overlay(
                    LinearGradient(
                        colors: [.clear, Design.Brand.accent, .clear],
                        startPoint: .leading, endPoint: .trailing
                    )
                    .frame(width: 90)
                    .offset(x: shimmerPhase * 90)
                    .blendMode(.plusLighter)
                )
                .mask(styled)
        }
    }

    private var headerLabel: String {
        if let duration, duration >= 1 { return "Thought for \(Int(duration.rounded()))s" }
        return "Thought process"
    }
}
