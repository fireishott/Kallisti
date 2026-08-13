import SwiftUI

/// A monospaced, dark, read-only terminal view that renders accumulated
/// live stdout for an in-flight tool. Chunks are appended in arrival order
/// and the view auto-scrolls to the bottom on new content unless the user
/// has manually scrolled up to read earlier output.
///
/// Used inside the Live tab of the canvas (CanvasView.liveActivityList),
/// one TerminalOutputView per ToolActivity row. Text is selectable so the
/// user can copy a command output. The view is intentionally cheap: plain
/// `Text` inside a `ScrollView` (no LazyVStack) so auto-scroll reliably
/// reaches the last line regardless of chunk cadence.
struct TerminalOutputView: View {
    /// The accumulated stdout — streamed in via `tool.output` events.
    let text: String
    /// Whether the tool is still running. Drives the caret indicator.
    let isActive: Bool
    /// Hard cap to keep the view from going unbounded on chatty tools.
    /// When the buffer exceeds this, the head is dropped and a truncation
    /// marker is shown in place. 64 KB is enough for ~3000 lines of
    /// average terminal output; anything beyond is ephemeral noise.
    let maxChars: Int

    @State private var pinnedToBottom: Bool = true
    @State private var lastSeenLength: Int = 0
    @State private var scrollProxy: ScrollViewProxy?

    private static let bottomAnchor = "terminal-bottom-anchor"

    init(
        text: String,
        isActive: Bool,
        maxChars: Int = 64 * 1024
    ) {
        self.text = text
        self.isActive = isActive
        self.maxChars = maxChars
    }

    private var displayText: String {
        if text.count <= maxChars { return text }
        // Drop the head, keep the tail, and prefix a marker so the user
        // knows where the visible buffer starts.
        let tail = text.suffix(maxChars)
        return "[… earlier output truncated …]\n" + tail
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().background(Design.Colors.border)
            scrollBacked
        }
        .background(terminalBackground)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(isActive ? "Live terminal output, streaming" : "Terminal output")
        .accessibilityValue(text)
        .onChange(of: text.count) { _, newCount in
            if newCount > lastSeenLength {
                lastSeenLength = newCount
                if pinnedToBottom {
                    scrollToBottom()
                }
            }
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Design.Spacing.xs) {
            Image(systemName: isActive ? "terminal.fill" : "terminal")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(isActive ? Design.Colors.success : Design.Colors.secondaryForeground)
            Text("STDOUT")
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .tracking(0.8)
                .foregroundStyle(Design.Colors.secondaryForeground)
            if isActive {
                Text("LIVE")
                    .font(.system(size: 9, weight: .bold, design: .monospaced))
                    .tracking(1.0)
                    .foregroundStyle(Design.Colors.success)
                    .padding(.horizontal, Design.Spacing.xxs)
                    .padding(.vertical, 1)
                    .background(
                        Capsule().fill(Design.Colors.success.opacity(0.15))
                    )
                    .overlay(
                        Capsule().stroke(Design.Colors.success.opacity(0.4), lineWidth: 0.5)
                    )
            }
            Spacer()
            Text("\(text.count) chars")
                .font(.system(size: 9, weight: .regular, design: .monospaced))
                .foregroundStyle(Design.Colors.tertiaryForeground)
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xxs)
        .background(Design.Colors.surface2)
    }

    // MARK: - Body

    private var scrollBacked: some View {
        ScrollViewReader { proxy in
            ScrollView(.vertical, showsIndicators: true) {
                // The view body is laid out as a single Text so the
                // engine measures it as one block. The trailing anchor is
                // rendered as a 1px Color so scrollTo can mark the bottom.
                VStack(alignment: .leading, spacing: 0) {
                    Text(displayText.isEmpty ? " " : displayText)
                        .font(.system(size: 11, weight: .regular, design: .monospaced))
                        .foregroundStyle(Design.Colors.foreground)
                        .textSelection(.enabled)
                        .lineSpacing(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, Design.Spacing.sm)
                        .padding(.top, Design.Spacing.xs)
                    Color.clear
                        .frame(height: 1)
                        .id(Self.bottomAnchor)
                }
            }
            .background(
                GeometryReader { geo in
                    Color.clear.preference(
                        key: TerminalScrollOffsetKey.self,
                        value: -geo.frame(in: .named("terminal-scroll")).minY
                    )
                }
            )
            .coordinateSpace(name: "terminal-scroll")
            .onPreferenceChange(TerminalScrollOffsetKey.self) { _ in
                // No-op: we rely on the tap-to-pin gesture below to break
                // auto-scroll. Reading the scroll offset here would force
                // a synchronous layout pass on every chunk.
            }
            .simultaneousGesture(
                // Any user-initiated drag breaks the pin-to-bottom
                // contract. We re-pin on scrollToBottom (called when the
                // user taps the "jump to bottom" affordance).
                DragGesture(minimumDistance: 2)
                    .onChanged { _ in
                        if pinnedToBottom { pinnedToBottom = false }
                    }
            )
            .onAppear {
                scrollProxy = proxy
                lastSeenLength = text.count
                if pinnedToBottom {
                    scrollToBottom()
                }
            }
        }
        .frame(minHeight: 80, maxHeight: 220)
    }

    // MARK: - Background

    private var terminalBackground: some View {
        // Near-black ink. Slightly greener than the surface so the terminal
        // reads as a distinct zone, even on dark themes.
        ZStack {
            Design.Colors.deepInk
            Color.green.opacity(0.02)
        }
    }

    // MARK: - Helpers

    private func scrollToBottom() {
        // Defer so the new content has been laid out before we ask the
        // ScrollView to scroll. SwiftUI's horizontal/vertical scroll proxy
        // ignores scrollTo calls issued before the content has settled.
        DispatchQueue.main.async {
            withAnimation(.linear(duration: 0.08)) {
                scrollProxy?.scrollTo(Self.bottomAnchor, anchor: .bottom)
            }
        }
    }
}

/// PreferenceKey used to detect scroll position. Static let per Swift 6
/// strict concurrency.
private struct TerminalScrollOffsetKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}
