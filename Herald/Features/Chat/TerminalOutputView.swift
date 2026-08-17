import SwiftUI

/// A monospaced, dark, live terminal view that renders accumulated stdout for
/// an in-flight tool. Chunks are appended in arrival order and the view
/// auto-scrolls to the bottom on new content unless the user has manually
/// scrolled up to read earlier output.
///
/// Build 128.56: restyled as a real terminal window - macOS-style traffic
/// lights in the title bar, near-black phosphor background, green-tinted live
/// text, and a blinking block caret while the tool is running. ANSI escape
/// sequences are stripped before display so raw tool output renders clean.
///
/// Used inside the Live tab of the canvas (CanvasView.liveActivityList) and
/// the live tool rail in chat (ToolActivityRail) - one TerminalOutputView per
/// ToolActivity row. Text is selectable so the user can copy command output.
/// The view is intentionally cheap: plain `Text` inside a `ScrollView` (no
/// LazyVStack) so auto-scroll reliably reaches the last line regardless of
/// chunk cadence.
struct TerminalOutputView: View {
    /// The accumulated stdout - streamed in via `tool.output` events.
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
    /// Build 128.59: minimize toggle collapses to just the title bar.
    @State private var isMinimized = false

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
        let cleaned = Self.strippingANSI(from: text)
        if cleaned.count <= maxChars { return cleaned }
        // Drop the head, keep the tail, and prefix a marker so the user
        // knows where the visible buffer starts.
        let tail = cleaned.suffix(maxChars)
        return "[… earlier output truncated …]\n" + tail
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            titleBar
            Divider()
                .background(Color.white.opacity(0.12))
            if !isMinimized { scrollBacked }
        }
        .background(terminalBackground)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.sm)
                .stroke(Color.white.opacity(0.1), lineWidth: 1)
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

    // MARK: - Title bar (macOS terminal traffic lights)

    private var titleBar: some View {
        HStack(spacing: Design.Spacing.xs) {
            // Traffic lights
            HStack(spacing: 5) {
                Circle().fill(Color(hex: 0xFF5F57)).frame(width: 9, height: 9)
                Circle().fill(Color(hex: 0xFEBC2E)).frame(width: 9, height: 9)
                Circle().fill(Color(hex: 0x28C840)).frame(width: 9, height: 9)
            }
            .padding(.leading, 2)

            Text("kallisti — stdout")
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundStyle(Color.white.opacity(0.55))
                .lineLimit(1)

            if isActive {
                Text("●")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundStyle(terminalGreen)
                    .accessibilityLabel("Streaming")
            }

            Spacer()

            Text("\(text.count) chars")
                .font(.system(size: 9, weight: .regular, design: .monospaced))
                .foregroundStyle(Color.white.opacity(0.35))

            // Build 128.59: minimize toggle.
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { isMinimized.toggle() }
            } label: {
                Image(systemName: isMinimized ? "chevron.up" : "chevron.down")
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(Color.white.opacity(0.45))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(isMinimized ? "Expand" : "Minimize")
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xxs)
        .background(Color.black.opacity(0.35))
    }

    // MARK: - Body (build 128.59: collapsible)

    private var scrollBacked: some View {
        ScrollViewReader { proxy in
            ScrollView(.vertical, showsIndicators: true) {
                // The view body is laid out as a single Text so the engine
                // measures it as one block. The trailing anchor is rendered as
                // a 1px Color so scrollTo can mark the bottom.
                VStack(alignment: .leading, spacing: 0) {
                    HStack(alignment: .firstTextBaseline, spacing: 0) {
                        Text(displayText.isEmpty ? " " : displayText)
                            .font(.system(size: 11, weight: .regular, design: .monospaced))
                            .foregroundStyle(terminalText)
                            .textSelection(.enabled)
                            .lineSpacing(1)

                        // Build 128.56: blinking block caret while streaming.
                        if isActive {
                            BlinkingCaret()
                                .padding(.leading, 2)
                        }
                    }
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
                // Any user-initiated drag breaks the pin-to-bottom contract.
                // We re-pin on scrollToBottom (called when the user taps the
                // "jump to bottom" affordance).
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
        // True terminal phosphor: near-black with a faint green cast, clearly
        // distinct from the app's chat surfaces.
        ZStack {
            Color.black
            terminalGreen.opacity(0.03)
        }
    }

    // MARK: - Terminal palette

    /// Classic terminal green - used for the live dot and the caret.
    private var terminalGreen: Color { Color(hex: 0x3DDC84) }
    /// Slightly green-tinted light text, like an OLED terminal.
    private var terminalText: Color { Color(hex: 0xC9E8D2) }

    // MARK: - ANSI stripping

    /// Remove ANSI/VT100 escape sequences from raw tool output so it renders
    /// as clean terminal text. Handles CSI sequences (`ESC [ ... letter`),
    /// OSC sequences (`ESC ] ... BEL`), and stray control bytes.
    static func strippingANSI(from raw: String) -> String {
        var out = raw
        // CSI: ESC [ params intermediate? final (0x40-0x7E)
        out = out.replacingOccurrences(
            of: #"\u{1B}\[[0-9;:?]*[ -/]*[@-~]"#,
            with: "",
            options: .regularExpression
        )
        // OSC: ESC ] ... (BEL or ST)
        out = out.replacingOccurrences(
            of: #"\u{1B}\][^\u{0007}\u{1B}]*(\u{0007}|\u{1B}\\)"#,
            with: "",
            options: .regularExpression
        )
        // Lone remaining ESC
        out = out.replacingOccurrences(of: "\u{1B}", with: "")
        // Other C0 control chars except newline/tab/CR
        out = out.replacingOccurrences(
            of: #"[\u{0000}-\u{0008}\u{000B}\u{000C}\u{000E}-\u{001F}]"#,
            with: "",
            options: .regularExpression
        )
        return out
    }

    // MARK: - Helpers

    private func scrollToBottom() {
        // Defer so the new content has been laid out before we ask the
        // ScrollView to scroll. SwiftUI's scroll proxy ignores scrollTo calls
        // issued before the content has settled.
        DispatchQueue.main.async {
            withAnimation(.linear(duration: 0.08)) {
                scrollProxy?.scrollTo(Self.bottomAnchor, anchor: .bottom)
            }
        }
    }
}

/// Build 128.56: a `▊` block caret that blinks on a 0.6s cycle. Pure SwiftUI -
/// no Timer, no animation retention issue under Reduce Motion.
private struct BlinkingCaret: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var visible = true

    var body: some View {
        Text("▊")
            .font(.system(size: 11, weight: .regular, design: .monospaced))
            .foregroundStyle(Color(hex: 0x3DDC84))
            .opacity(reduceMotion ? 1 : (visible ? 1 : 0.15))
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.easeInOut(duration: 0.6).repeatForever(autoreverses: true)) {
                    visible = false
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