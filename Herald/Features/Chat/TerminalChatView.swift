import SwiftUI

/// Terminal chat transcript - the real TUI experience (Build 128.75).
///
/// When Chat Display is "Terminal", the message list renders like a native
/// terminal agent session instead of rich bubbles:
///   - Assistant turns are gold-bordered agent blocks with a `Hermes` label
///     and wall-clock timestamp (same visual DNA as the Hermes TUI).
///   - Reasoning renders under a `Reasoning` header with a rule, dim italic
///     text, and a `[thought for Xs]` footer when done.
///   - Tool calls render as `$` command lines with a live elapsed timer while
///     active and a duration when finished; stdout streams inside the block.
///   - A sticky status bar at the bottom shows model, token usage vs context
///     window with a progress bar, and session elapsed time.
///   - User messages render as green `user@hermes:~$` prompt lines.
///
/// The composer, banners, and queue bar remain untouched - this view only
/// replaces the transcript renderer. Data stays identical to rich mode:
/// the same Message objects, same transcriptRows ordering, same auto-scroll.
struct TerminalChatView: View {
    let messages: [Message]
    let showReasoning: Bool
    let onRetry: (Message) -> Void

    /// Model name for the status bar (from ModelStore/chatStore/hostStore).
    var modelName: String?
    /// Context window (tokens) for the status bar progress.
    var contextWindow: Int?
    /// Current context tokens consumed this session (0 when unknown).
    var contextTokens: Int = 0

    @State private var scrollProxy: ScrollViewProxy?
    @State private var isUserScrolling = false
    @State private var autoScrollTask: Task<Void, Never>?
    @State private var sessionStart = Date.now
    private static let scrollDebounceInterval: Duration = .milliseconds(100)

    private var rows: [Message] {
        ChatScreen.transcriptRows(messages)
    }

    private var firstMessageTimestamp: Date? {
        rows.first?.timestamp
    }

    /// Session elapsed time shown on the status bar. Anchored to the first
    /// message's timestamp so the clock survives view recreation (switching
    /// chats, scrolling, tab changes) instead of resetting to .now.
    private var sessionEpoch: Date {
        firstMessageTimestamp ?? sessionStart
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView(.vertical) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        Color.clear
                            .frame(height: 1)
                            .id("top")

                        if rows.isEmpty {
                            emptyState
                        } else {
                            ForEach(rows) { message in
                                TerminalMessageRow(
                                    message: message,
                                    showReasoning: showReasoning,
                                    onRetry: onRetry
                                )
                                .id(message.id)
                            }
                        }

                        Color.clear
                            .frame(height: 12)
                            .id("bottom")
                    }
                    .padding(.vertical, Design.Spacing.sm)
                }
                .scrollDismissesKeyboard(.interactively)
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    let bottomY = geometry.contentOffset.y + geometry.visibleRect.size.height
                    let contentBottom = geometry.contentSize.height + geometry.contentInsets.bottom
                    return bottomY >= contentBottom - 44
                } action: { _, nearBottom in
                    if nearBottom {
                        isUserScrolling = false
                    }
                }
                .simultaneousGesture(
                    DragGesture()
                        .onChanged { value in
                            if value.translation.height != 0 {
                                isUserScrolling = true
                            }
                        }
                )
                .onChange(of: rows.count) { _, _ in
                    scrollToBottom(force: true, animate: true)
                }
                .onChange(of: rows.last?.streamingCompositeID) { _, _ in
                    scrollToBottom(force: false, animate: false)
                }
                .background(TerminalPalette.background)
                .onAppear {
                    scrollProxy = proxy
                    scrollToBottom(force: true, animate: false)
                }
            }

            statusBar
        }
        .background(TerminalPalette.background)
    }

    // MARK: - Status bar (TUI bottom strip)

    private var statusBar: some View {
        VStack(spacing: 0) {
            Rectangle()
                .fill(TerminalPalette.border)
                .frame(height: 1)

            HStack(spacing: 8) {
                // Model
                HStack(spacing: 5) {
                    Image(systemName: "cpu")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(TerminalPalette.green)
                    Text(modelName ?? "hermes")
                        .font(.system(size: 10, weight: .medium, design: .monospaced))
                        .foregroundStyle(TerminalPalette.foreground)
                        .lineLimit(1)
                }

                Rectangle()
                    .fill(TerminalPalette.border)
                    .frame(width: 1, height: 10)

                // Token usage + progress
                if let contextWindow, contextWindow > 0 {
                    let used = min(contextTokens, contextWindow)
                    let pct = Double(used) / Double(contextWindow)

                    Text(Self.compactTokens(contextTokens) + "/" + Self.compactTokens(contextWindow))
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(TerminalPalette.dim)
                        .lineLimit(1)

                    GeometryReader { geo in
                        ZStack(alignment: .leading) {
                            Capsule()
                                .fill(TerminalPalette.border)
                            Capsule()
                                .fill(TerminalPalette.green)
                                .frame(width: max(2, geo.size.width * pct))
                        }
                    }
                    .frame(width: 64, height: 4)

                    Text("\(Int(pct * 100))%")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(TerminalPalette.dim)
                        .lineLimit(1)
                } else {
                    Text("context n/a")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(TerminalPalette.faint)
                        .lineLimit(1)
                }

                Spacer(minLength: 0)

                // Session elapsed time
                TimelineView(.periodic(from: sessionEpoch, by: 1)) { context in
                    Text(Self.sessionClock(seconds: context.date.timeIntervalSince(sessionEpoch)))
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(TerminalPalette.faint)
                        .lineLimit(1)
                        .monospacedDigit()
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, 5)
            .background(TerminalPalette.statusBar)
        }
    }

    private static func compactTokens(_ n: Int) -> String {
        if n >= 1_000_000 {
            let v = Double(n) / 1_000_000
            return String(format: v >= 10 ? "%.0fM" : "%.1fM", v)
        }
        if n >= 1_000 {
            let v = Double(n) / 1_000
            return String(format: v >= 10 ? "%.0fK" : "%.1fK", v)
        }
        return "\(n)"
    }

    private static func sessionClock(seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds))
        let h = s / 3600
        let m = (s % 3600) / 60
        let sec = s % 60
        if h > 0 { return String(format: "%dh %02dm", h, m) }
        if m > 0 { return String(format: "%dm %02ds", m, sec) }
        return String(format: "%ds", sec)
    }

    // MARK: - Empty state

    private static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.2.6"
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("kallisti - hermes agent \(Self.appVersion)")
                .font(Design.Typography.code)
                .foregroundStyle(TerminalPalette.green)
            Text("connected. type a message below to start a session.")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.dim)
            Text("cli chat mode: settings > preferences > chat display")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.faint)
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.top, Design.Spacing.lg)
        .textSelection(.enabled)
    }

    // MARK: - Auto-scroll

    private func scrollToBottom(force: Bool, animate: Bool) {
        guard force || !isUserScrolling else { return }
        autoScrollTask?.cancel()
        autoScrollTask = Task { @MainActor in
            if isStreaming {
                try? await Task.sleep(for: Self.scrollDebounceInterval)
                guard !Task.isCancelled, (force || !isUserScrolling) else { return }
            }
            for _ in 0..<4 {
                guard !Task.isCancelled else { return }
                if animate {
                    withAnimation(Design.Motion.standard) {
                        scrollProxy?.scrollTo("bottom", anchor: .bottom)
                    }
                } else {
                    scrollProxy?.scrollTo("bottom", anchor: .bottom)
                }
                try? await Task.sleep(for: .milliseconds(70))
            }
        }
    }

    private var isStreaming: Bool {
        rows.contains { $0.isStreaming }
    }
}

/// One message rendered as terminal lines (Build 128.75 TUI restyle).
private struct TerminalMessageRow: View {
    let message: Message
    let showReasoning: Bool
    let onRetry: (Message) -> Void

    private static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm"
        return f
    }()

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if message.sender == .user || message.sender == .voiceUser {
                userLine
            } else if message.sender == .system {
                systemLine
            } else {
                heraldBlock
            }
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, 3)
        .textSelection(.enabled)
    }

    // MARK: - User prompt

    private var userLine: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text("user@hermes:~$ ")
                .font(Design.Typography.code)
                .foregroundStyle(TerminalPalette.green)
            Text(cleanedUserContent)
                .font(Design.Typography.code)
                .foregroundStyle(TerminalPalette.foreground)
            if message.status == .sending {
                Text(" ▊")
                    .font(Design.Typography.code)
                    .foregroundStyle(TerminalPalette.green)
            } else if message.status == .failed || message.status == .interrupted {
                Text(" ✗")
                    .font(Design.Typography.code)
                    .foregroundStyle(TerminalPalette.error)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private var cleanedUserContent: String {
        let content = message.content.trimmingCharacters(in: .whitespacesAndNewlines)
        if content.isEmpty, !message.attachments.isEmpty {
            return message.attachments.map { $0.fileName }.joined(separator: " ")
        }
        return content
    }

    // MARK: - System line

    private var systemLine: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Text("[" + Self.systemLabel(message) + "]")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.dim)
            Text(message.content.trimmingCharacters(in: .whitespacesAndNewlines))
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.dim)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private static func systemLabel(_ message: Message) -> String {
        if message.content.contains("[Voice session ended]") {
            return "voice"
        }
        if message.content.contains("[Voice") || message.content.contains("[voice") {
            return "voice"
        }
        return "system"
    }

    // MARK: - Assistant block (TUI gold-bordered agent card)

    private var heraldBlock: some View {
        VStack(alignment: .leading, spacing: 0) {
            headerRow

            VStack(alignment: .leading, spacing: 4) {
                // Reasoning header + dim italic chain-of-thought.
                if showReasoning, !message.reasoning.isEmpty {
                    reasoningSection
                }

                // Tool activities as command blocks with live timers.
                if !message.toolActivities.isEmpty {
                    toolSection
                }

                // Attachments.
                if !message.attachments.isEmpty {
                    attachmentLine
                }

                // Final answer text.
                if !displayContent.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: 0) {
                        Text(displayContent)
                            .font(Design.Typography.code)
                            .foregroundStyle(TerminalPalette.foreground)
                        if message.isStreaming {
                            StreamingBlockCaret()
                        }
                    }
                    .fixedSize(horizontal: false, vertical: true)
                } else if message.isStreaming && message.reasoning.isEmpty && message.toolActivities.isEmpty {
                    // Streaming but no visible content yet - show the caret alone.
                    HStack(alignment: .firstTextBaseline, spacing: 0) {
                        Text("▊")
                            .font(Design.Typography.code)
                            .foregroundStyle(TerminalPalette.green)
                    }
                }

                // Failure state.
                if message.status == .failed || message.status == .interrupted {
                    failureLine
                }
            }
            .padding(.leading, Design.Spacing.md)
            .padding(.top, 6)
            .padding(.bottom, Design.Spacing.sm)
        }
        .padding(.leading, 6)
        .overlay(alignment: .leading) {
            // Gold spine - the TUI's agent border on the left edge.
            Rectangle()
                .fill(terminalAccent)
                .frame(width: 2)
        }
    }

    private var terminalAccent: Color {
        if message.status == .failed || message.status == .interrupted {
            return TerminalPalette.error
        }
        return message.isStreaming ? TerminalPalette.gold : TerminalPalette.borderGold
    }

    private var headerRow: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "sparkles")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(terminalAccent)
            Text("Hermes")
                .font(.system(size: 11, weight: .bold, design: .monospaced))
                .foregroundStyle(terminalAccent)
            Rectangle()
                .fill(terminalAccent.opacity(0.35))
                .frame(height: 1)
            Text(Self.formatter.string(from: message.timestamp))
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(TerminalPalette.faint)
        }
        .padding(.top, 5)
        .padding(.leading, Design.Spacing.sm)
        .padding(.trailing, Design.Spacing.sm)
    }

    // MARK: - Reasoning section

    private var reasoningSection: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text("Reasoning")
                    .font(.system(size: 10, weight: .semibold, design: .monospaced))
                    .foregroundStyle(TerminalPalette.reasoningHeader)
                Rectangle()
                    .fill(TerminalPalette.border)
                    .frame(height: 1)
            }

            ForEach(reasoningParsedLines, id: \.self) { line in
                Text(line)
                    .font(.system(size: 11, design: .monospaced))
                    .italic()
                    .foregroundStyle(TerminalPalette.reasoning)
                    .lineLimit(nil)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if message.isStreaming {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text("[thinking]")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(TerminalPalette.reasoningHeader)
                    StreamingBlockCaret()
                }
            } else if let duration = message.reasoningDuration {
                Text("[thought for \(Int(duration))s]")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(TerminalPalette.faint)
            }
        }
        .padding(.bottom, 2)
    }

    private var reasoningParsedLines: [String] {
        let cleaned = TerminalPalette.cleaned(message.reasoning)
        return cleaned.components(separatedBy: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    // MARK: - Tool section

    private var toolSection: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(message.toolActivities) { activity in
                TerminalToolLine(activity: activity)
            }
        }
        .padding(.bottom, 2)
    }

    private var attachmentLine: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Text("📎 " + message.attachments.map { $0.fileName }.joined(separator: ", "))
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.dim)
        }
        .padding(.bottom, 2)
    }

    private var displayContent: String {
        let content = Self.cleanedDirectives(message.content)
        // Skip the auto-generated attachment placeholder text.
        if !message.attachments.isEmpty,
           content.range(of: #"^\[\d+ attachment"#, options: .regularExpression) != nil {
            return ""
        }
        return content
    }

    /// Mirror MessageBubble.stripImageDirectives: remove Hermes inline
    /// directives (@file:..., @image:[...], @diff, etc.) so terminal text
    /// reads clean instead of showing raw directive syntax.
    private static func cleanedDirectives(_ content: String) -> String {
        let pattern = #"(?<![\\w/])@(?:(?:diff|staged)\b|image:\[[^\]\n]*\](?:\{name=[^}\n]*\})?|(?:file|folder|git|url|image|tool|line|terminal|session):(`[^`\n]+`|"[^"\n]+"|'[^'\n]+'|\S+)(?::\d+(?:-\d+)?(?=$|[\s,;\}\]"'\)`]))?(?:\{name=[^}\n]*\})?)"#
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

    private var failureLine: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Text("✗ " + (message.errorCategory ?? message.status.rawValue) + " (tap to retry)")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.error)
            Button {
                onRetry(message)
            } label: {
                Text("[retry]")
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.green)
            }
            .buttonStyle(.plain)
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// One tool activity as a TUI command block: `$` prompt line with a live
/// elapsed timer while active, duration when finished, stdout streaming
/// inside a bordered window, and a one-line result note when done.
private struct TerminalToolLine: View {
    let activity: ToolActivity

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("$")
                    .font(Design.Typography.code)
                    .foregroundStyle(TerminalPalette.green)
                Text(activity.label)
                    .font(Design.Typography.code)
                    .foregroundStyle(TerminalPalette.cyan)
                if let args = activity.argsPreview, !args.isEmpty {
                    Text(Self.truncate(args))
                        .font(Design.Typography.codeSmall)
                        .foregroundStyle(TerminalPalette.dim)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                timer
            }
            .fixedSize(horizontal: false, vertical: true)

            if activity.isActive, !activity.liveOutput.isEmpty {
                TerminalOutputView(text: activity.liveOutput, isActive: true, maxChars: 32 * 1024)
                    .padding(.leading, Design.Spacing.xs)
            } else if !activity.isActive, let result = activity.resultPreview, !result.isEmpty {
                Text(Self.truncateMulti(result))
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.dim)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, Design.Spacing.xs)
            }
        }
        .padding(6)
        .overlay(
            RoundedRectangle(cornerRadius: 4)
                .stroke(activity.isActive ? TerminalPalette.gold.opacity(0.5) : TerminalPalette.border, lineWidth: 1)
        )
    }

    /// Live elapsed timer (TUI style: `0.0s`) while active via TimelineView;
    /// static `Xms` duration when finished.
    @ViewBuilder
    private var timer: some View {
        if activity.isActive {
            TimelineView(.periodic(from: activity.startedAt, by: 0.1)) { context in
                Text(String(format: "%.1fs", max(0, context.date.timeIntervalSince(activity.startedAt))))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(TerminalPalette.gold)
                    .monospacedDigit()
            }
        } else if let ms = activity.durationMs {
            Text(ms >= 1000 ? String(format: "%.1fs", Double(ms) / 1000.0) : "\(ms)ms")
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(TerminalPalette.faint)
                .monospacedDigit()
        }
    }

    private static func truncate(_ s: String) -> String {
        let cleaned = TerminalPalette.cleaned(s)
        // Flatten newlines so the args stay on one line after the tool name.
        let flat = cleaned.replacingOccurrences(of: "\n", with: " ")
        if flat.count > 90 { return String(flat.prefix(90)) + "…" }
        return flat
    }

    private static func truncateMulti(_ s: String) -> String {
        let cleaned = TerminalPalette.cleaned(s)
        if cleaned.count > 240 { return String(cleaned.prefix(240)) + "…" }
        return cleaned
    }
}

/// Blinking block caret for a live streaming row. Mirrors the TerminalOutputView
/// caret: pure SwiftUI, 0.6s cycle, static under Reduce Motion.
struct StreamingBlockCaret: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var visible = true

    var body: some View {
        Text("▊")
            .font(Design.Typography.code)
            .foregroundStyle(TerminalPalette.green)
            .opacity(reduceMotion ? 1 : (visible ? 1 : 0.15))
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.easeInOut(duration: 0.6).repeatForever(autoreverses: true)) {
                    visible = false
                }
            }
            .onDisappear {
                visible = true
            }
    }
}

/// Shared terminal palette for the TUI experience - near-black navy ground,
/// green + cyan accents, gold agent borders, amber reasoning, dim/faint
/// secondary levels. Matches the Hermes TUI visual DNA.
enum TerminalPalette {
    static let background = Color(hex: 0x0B0E14)
    static let statusBar = Color(hex: 0x0D1117)
    static let foreground = Color(hex: 0xD8E1E8)
    static let green = Color(hex: 0x3DDC84)
    static let cyan = Color(hex: 0x5AC8FA)
    static let gold = Color(hex: 0xE5C07B)
    static let borderGold = Color(hex: 0x8A7038)
    static let reasoning = Color(hex: 0xA8B0BA)
    static let reasoningHeader = Color(hex: 0xE5C07B)
    static let border = Color(hex: 0x2A3440)
    static let dim = Color(hex: 0x7A8B80)
    static let faint = Color(hex: 0x46534B)
    static let error = Color(hex: 0xFF6B6B)

    /// Strip ANSI/VT100 + stray control bytes so raw tool text renders clean.
    /// Shared with TerminalOutputView so both surfaces behave identically.
    static func cleaned(_ raw: String) -> String {
        TerminalOutputView.strippingANSI(from: raw)
    }
}