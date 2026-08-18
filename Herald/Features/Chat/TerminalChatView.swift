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
    /// Build 128.76: available slash commands/tools for the TUI landing
    /// panel (chatStore.commandCatalog names). Empty hides the panel.
    var availableCommands: [String] = []
    /// Build 128.76: available skills/profile names for the TUI landing
    /// panel. Empty hides the panel.
    var availableSkills: [String] = []
    /// Build 128.76: session id shown in the TUI header info block.
    var sessionLabel: String? = nil
    /// Build 128.78: whether the terminal prompt can accept input. Mirrors
    /// the rich composer's connection gate (online/degraded = enabled).
    var inputEnabled: Bool = true
    /// Build 128.78: sends a line typed at the terminal prompt. ChatScreen
    /// routes this through the same enqueue + slash-command path as the
    /// rich composer - this IS the real input for the TUI, not a skin.
    var onSendText: (String) -> Void = { _ in }

    @State private var scrollProxy: ScrollViewProxy?
    @State private var isUserScrolling = false
    @State private var autoScrollTask: Task<Void, Never>?
    @State private var sessionStart = Date.now
    // Build 128.78: terminal prompt state - the TUI owns its own input.
    @State private var promptText = ""
    @State private var promptHistory: [String] = []
    @State private var historyIndex: Int? = nil
    @State private var promptFocused = false
    @FocusState private var promptFocusedField: Bool
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
            terminalInputBar
        }
        .background(TerminalPalette.background)
    }

    // MARK: - Terminal input (Build 128.78)

    /// The real TUI prompt - a focused text field with a `> ` prompt prefix,
    /// slash-command autocomplete from the live command catalog, and up/down
    /// arrow history. Submitting routes through onSendText (ChatScreen's
    /// enqueue + slash dispatcher), so typing here behaves exactly like the
    /// CLI: plain text sends a message, /commands execute locally or pass
    /// through to the agent.
    private var terminalInputBar: some View {
        VStack(spacing: 0) {
            Rectangle()
                .fill(TerminalPalette.border)
                .frame(height: 1)

            VStack(alignment: .leading, spacing: 4) {
                // Autocomplete strip for /commands
                if promptText.hasPrefix("/"), !promptText.hasSuffix(" "), let suggestions = commandSuggestions, !suggestions.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(suggestions, id: \.self) { suggestion in
                                Button {
                                    promptText = "/" + suggestion + " "
                                    promptFocusedField = true
                                } label: {
                                    Text("/" + suggestion)
                                        .font(.system(size: 12, design: .monospaced))
                                        .foregroundStyle(TerminalPalette.cyan)
                                        .padding(.horizontal, 8)
                                        .padding(.vertical, 3)
                                        .background(
                                            RoundedRectangle(cornerRadius: 4)
                                                .fill(TerminalPalette.cyan.opacity(0.12))
                                        )
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    .frame(height: 26)
                }

                HStack(spacing: 0) {
                    Text("> ")
                        .font(.system(size: 14, weight: .semibold, design: .monospaced))
                        .foregroundStyle(TerminalPalette.gold)

                    TextField("type a message or /command", text: $promptText)
                        .font(.system(size: 14, design: .monospaced))
                        .foregroundStyle(
                            inputEnabled ? TerminalPalette.foreground : TerminalPalette.faint
                        )
                        .focused($promptFocusedField)
                        .disabled(!inputEnabled)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .submitLabel(.send)
                        .onSubmit { submitPrompt() }
                        .onChange(of: promptText) { _, newValue in
                            if newValue.contains("\n") {
                                promptText = newValue.replacingOccurrences(of: "\n", with: "")
                            }
                            historyIndex = nil
                        }
                        .onKeyPress(.upArrow) {
                            navigateHistory(-1)
                            return .handled
                        }
                        .onKeyPress(.downArrow) {
                            navigateHistory(1)
                            return .handled
                        }

                    if !promptText.isEmpty {
                        Text("▊")
                            .font(.system(size: 14, design: .monospaced))
                            .foregroundStyle(TerminalPalette.green)
                    } else {
                        Text("▊")
                            .font(.system(size: 14, design: .monospaced))
                            .foregroundStyle(TerminalPalette.gold)
                            .opacity(0.9)
                    }
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, 8)
            .background(TerminalPalette.statusBar)
        }
    }

    /// Prefix-matched command suggestions for the TUI autocomplete strip.
    private var commandSuggestions: [String]? {
        let typed = String(promptText.dropFirst())
        guard !typed.isEmpty else { return availableCommands.prefix(8).map { $0 } }
        return availableCommands
            .filter { $0.lowercased().hasPrefix(typed.lowercased()) }
            .prefix(8)
            .map { $0 }
    }

    private func submitPrompt() {
        let line = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty else { return }
        promptHistory.append(line)
        historyIndex = nil
        promptText = ""
        onSendText(line)
    }

    private func navigateHistory(_ direction: Int) {
        guard !promptHistory.isEmpty else { return }
        let newIndex: Int
        if let idx = historyIndex {
            newIndex = min(max(idx + direction, 0), promptHistory.count - 1)
        } else {
            newIndex = direction < 0 ? promptHistory.count - 1 : 0
        }
        historyIndex = newIndex
        promptText = promptHistory[newIndex]
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

    // MARK: - Empty state (TUI landing, Build 128.76)

    private static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.2.6"
    }

    /// Block-letter ASCII banner, rootshell/Hermes-TUI style. Rendered in
    /// gold on the landing screen so it reads like the real agent TUI
    /// rather than a chat placeholder.
    private static let bannerLines: [String] = [
        " _  __    _    _     _    ___ _____ _____ ",
        "| |/ /   / \\  | |   | |  / _ \\_   _|_   _|",
        "| ' /   / _ \\ | |   | | | | | || |   | |  ",
        "| . \\  / ___ \\| |___| |_| |_| || |   | |  ",
        "|_|\\_\\/_/   \\_\\_____|_\\___/___||_|   |_|  ",
    ]

    private var banner: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(Self.bannerLines.enumerated()), id: \.offset) { _, line in
                Text(line)
                    .font(.system(size: 13, weight: .bold, design: .monospaced))
                    .foregroundStyle(TerminalPalette.gold)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .scaleEffect(x: 1.0, y: 0.9, anchor: .leading)
            }
            Text("kallisti / hermes agent  " + Self.appVersion)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(TerminalPalette.green)
                .padding(.top, 2)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Kallisti terminal. Hermes agent \(Self.appVersion).")
    }

    /// Session info block (model, session id, tokens) on the landing screen.
    private var sessionInfo: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("─ Session ─")
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundStyle(TerminalPalette.gold)
            Text("model    \(modelName ?? "hermes")")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.foreground)
            if let sessionLabel {
                Text("session  \(sessionLabel)")
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.dim)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Text("context  \(Self.compactTokens(contextTokens)) / \(contextWindow.map(Self.compactTokens) ?? "?")")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.dim)
            Text("status   connected")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.green)
        }
        .padding(Design.Spacing.sm)
        .overlay(
            RoundedRectangle(cornerRadius: 4)
                .stroke(TerminalPalette.border, lineWidth: 1)
        )
    }

    /// Panel listing available slash commands (from the live command catalog).
    private var toolsPanel: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("─ Available Commands ─")
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundStyle(TerminalPalette.gold)
            let shown = Array(availableCommands.prefix(12))
            if shown.isEmpty {
                Text("(catalog loading…)")
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.faint)
            } else {
                Text(shown.joined(separator: "  "))
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.cyan)
                    .fixedSize(horizontal: false, vertical: true)
                if availableCommands.count > 12 {
                    Text("…and \(availableCommands.count - 12) more")
                        .font(Design.Typography.codeSmall)
                        .foregroundStyle(TerminalPalette.faint)
                }
            }
        }
        .padding(Design.Spacing.sm)
        .overlay(
            RoundedRectangle(cornerRadius: 4)
                .stroke(TerminalPalette.border, lineWidth: 1)
        )
    }

    /// Panel listing available profiles (the closest runtime skill surface this
    /// device reports). Renamed deliberately - showing profile names under a
    /// "Skills" header would be a lie; the Hub skills bug (b85-b96) is
    /// documented and the profile list is the honest available data.
    private var skillsPanel: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("─ Profiles ─")
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundStyle(TerminalPalette.gold)
            let shown = Array(availableSkills.prefix(10))
            if shown.isEmpty {
                Text("(no profiles loaded)")
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.faint)
            } else {
                Text(shown.joined(separator: "  "))
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.dim)
                    .fixedSize(horizontal: false, vertical: true)
                if availableSkills.count > 10 {
                    Text("…and \(availableSkills.count - 10) more")
                        .font(Design.Typography.codeSmall)
                        .foregroundStyle(TerminalPalette.faint)
                }
            }
        }
        .padding(Design.Spacing.sm)
        .overlay(
            RoundedRectangle(cornerRadius: 4)
                .stroke(TerminalPalette.border, lineWidth: 1)
        )
    }

    /// Blinking block caret used by the prompt line and streaming rows.
    /// Shared so the landing prompt and live streaming share one visual
    /// language (pure SwiftUI, 0.6s cycle, static under Reduce Motion).
    private struct BlockCaret: View {
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @State private var visible = true

        var body: some View {
            Text("█")
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(TerminalPalette.green)
                .opacity(reduceMotion ? 1 : (visible ? 1 : 0.15))
                .onAppear {
                    guard !reduceMotion else { return }
                    withAnimation(.easeInOut(duration: 0.6).repeatForever(autoreverses: true)) {
                        visible = false
                    }
                }
                .onDisappear { visible = true }
        }
    }

    /// Prompt line for the landing screen (`> ` + caret).
    private var promptLine: some View {
        HStack(spacing: 0) {
            Text("> ")
                .font(.system(size: 13, weight: .bold, design: .monospaced))
                .foregroundStyle(TerminalPalette.gold)
            BlockCaret()
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.md) {
            banner
            sessionInfo
            toolsPanel
            skillsPanel
            promptLine
            Text("type at the > prompt below, or /help for commands. switch back to rich chat: settings > preferences > chat display")
                .font(Design.Typography.codeSmall)
                .foregroundStyle(TerminalPalette.faint)
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.top, Design.Spacing.lg)
        .padding(.bottom, Design.Spacing.md)
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

    // MARK: - Assistant block (flat TUI text - no card chrome)

    private var heraldBlock: some View {
        VStack(alignment: .leading, spacing: 3) {
            // Reasoning header + dim italic chain-of-thought.
            if showReasoning, !message.reasoning.isEmpty {
                reasoningSection
            }

            // Tool activities as flat command lines with live timers.
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
        VStack(alignment: .leading, spacing: 2) {
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
            } else if !activity.isActive, let result = activity.resultPreview, !result.isEmpty {
                Text(Self.truncateMulti(result))
                    .font(Design.Typography.codeSmall)
                    .foregroundStyle(TerminalPalette.dim)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
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