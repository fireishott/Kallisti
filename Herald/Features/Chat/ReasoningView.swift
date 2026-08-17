import SwiftUI
import UIKit

/// Displays a Herald message's streamed reasoning / chain-of-thought.
///
/// While the answer streams, reasoning shows as a live tail: the newest lines at
/// full weight, older lines fading toward the top of a clipped viewport, under a
/// shimmering "Thinking… Xs" header.  Once the answer arrives it collapses to a
/// tappable "Thought for Xs" row.
///
/// Build 128.56: two-level expand. The default small viewport (132pt) auto-expands
/// when the turn starts and collapses when it finishes. A dedicated expand control
/// in the header switches to a large monitor viewport (roughly half the screen
/// height) so the live reasoning is easy to read while it streams; tapping the
/// header chevron still collapses/expands the small view.
struct ReasoningView: View {
    let reasoning: String
    let isStreaming: Bool
    let duration: TimeInterval?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isExpanded = false
    @State private var isLarge = false
    @State private var shimmerPhase: CGFloat = -1
    @State private var startedAt: Date = .now

    private let streamingViewportHeight: CGFloat = 132
    /// Build 128.56: large monitor viewport - ~52% of the screen height so a
    /// long reasoning tail can be watched without fighting the chat layout.
    private var largeViewportHeight: CGFloat {
        max(240, UIScreen.main.bounds.height * 0.52)
    }

    private var viewportHeight: CGFloat {
        isLarge ? largeViewportHeight : streamingViewportHeight
    }

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
                    isLarge = false
                }
            }
        }
        .animation(reduceMotion ? nil : Design.Motion.standard, value: isLarge)
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
                ScrollView(.vertical, showsIndicators: true) {
                    content
                }
                .frame(maxHeight: viewportHeight)
                .mask(
                    // Build 128.56: fade the OLD lines only in the small view.
                    // In the large viewport the whole tail stays readable so the
                    // user can actually monitor the stream.
                    isLarge
                        ? nil
                        : LinearGradient(
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
            // Build 128.61: completed thoughts need the same height-capped,
            // scroll-backed viewport as live thoughts. Previously the body
            // rendered UNBOUNDED (full height), so the resize button toggled
            // isLarge with nothing to resize - it "stayed expanded" forever.
            // No fade mask here: a finished thought is final, users scroll it.
            ScrollView(.vertical, showsIndicators: true) {
                content
            }
            .frame(maxHeight: viewportHeight)
            .transition(.opacity.combined(with: .move(edge: .top)))
        }
    }

    /// Newest line full strength; each earlier line steps down to a 0.4 floor.
    private func opacity(for index: Int, of total: Int) -> Double {
        guard isStreaming, total > 1 else { return 0.85 }
        let distance = Double(total - 1 - index)
        return max(0.4, 0.95 - distance * 0.11)
    }

    private var showBody: Bool { isExpanded }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Design.Spacing.xxs) {
            Button {
                // Collapse / expand the small view (chevron). Also collapses a
                // large view back to the small viewport.
                withAnimation(reduceMotion ? nil : Design.Motion.standard) {
                    isExpanded.toggle()
                    if isLarge { isLarge = false }
                }
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
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(isStreaming ? "Kallisti is thinking (tap to collapse or expand)" : headerLabel)

            Spacer(minLength: 0)

            // Build 128.56: dedicated expand control - flips between the small
            // viewport and the large monitor viewport. Enabled whenever the body
            // is showing (or a stream is in flight and auto-expanded).
            if showBody || (isStreaming && isExpanded) {
                Button {
                    withAnimation(reduceMotion ? nil : Design.Motion.standard) {
                        isLarge.toggle()
                        if isLarge { isExpanded = true }
                    }
                } label: {
                    Image(systemName: isLarge ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(isLarge ? Design.Brand.accent : Design.Colors.secondaryForeground)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(isLarge ? "Shrink thinking bubble" : "Expand thinking bubble")
            }

            // Chevron: reflects the small-view collapse state.
            Image(systemName: showBody ? "chevron.down" : "chevron.right")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(isStreaming ? Design.Brand.accent : Design.Colors.secondaryForeground)
                .frame(width: 16, height: 28)
                .contentShape(Rectangle())
                .onTapGesture {
                    withAnimation(reduceMotion ? nil : Design.Motion.standard) {
                        isExpanded.toggle()
                        if isLarge { isLarge = false }
                    }
                }
                .accessibilityLabel(showBody ? "Collapse thinking details" : "Expand thinking details")
        }
        .padding(.vertical, Design.Spacing.xxs)
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

#Preview {
    VStack {
        ReasoningView(
            reasoning: "Line one\nLine two\nLine three\nLine four\nLine five\nLine six\nLine seven\nLine eight",
            isStreaming: true,
            duration: nil
        )
        Spacer()
    }
    .padding()
    .background(Design.Colors.background)
}