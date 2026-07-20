import SwiftUI

/// Right-side inspector panel for iPad.
/// Shows Herald engine logs, terminal output, and usage —
/// similar to the right panel in Herald Desktop.
/// Width is owned by the parent workspace; this view fills its allocation.
struct iPadRightPanelView: View {
    @Environment(HeraldHostStore.self) private var hostStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(HeraldCanvasStore.self) private var canvasStore
    @Binding var isOpen: Bool
    @Binding var selectedTab: RightPanelTab
    @State private var selectedLogLevel: LogLevel? = nil

    var body: some View {
        if !isOpen { return AnyView(EmptyView()) }

        return AnyView(
            VStack(spacing: 0) {
                tabBar

                switch selectedTab {
                case .logs:     logsContent
                case .terminal: terminalContent
                case .tools:    toolsContent
                case .canvas:   CanvasView(store: canvasStore)
                }
            }
            .frame(maxWidth: .infinity)
            .background(Design.Colors.surface)
        )
    }

    // MARK: - Tab Bar

    private var tabBar: some View {
        HStack(spacing: 0) {
            ForEach(RightPanelTab.allCases) { tab in
                Button {
                    selectedTab = tab
                } label: {
                    VStack(spacing: 4) {
                        Image(systemName: tab.icon)
                            .font(.system(size: 13))
                        Text(tab.title)
                            .font(.system(size: 10, weight: .medium))
                    }
                    .foregroundStyle(selectedTab == tab ? Design.Brand.accent : Design.Colors.secondaryForeground)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, Design.Spacing.sm)
                }
                .buttonStyle(.plain)
            }

            Button {
                withAnimation(Design.Motion.standard) { isOpen = false }
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .padding(.horizontal, Design.Spacing.sm)
            }
            .buttonStyle(.plain)
        }
        .padding(.top, Design.Spacing.xs)
        .background(Design.Colors.background)
    }

    // MARK: - Logs

    private var filteredLogEntries: [LogEntry] {
        guard let level = selectedLogLevel else { return chatStore.logEntries }
        return chatStore.logEntries.filter { $0.level == level }
    }

    private var logsContent: some View {
        VStack(spacing: 0) {
            logFilterBar

            let entries = filteredLogEntries
            if entries.isEmpty {
                emptyState(icon: "terminal", message: "No log entries yet",
                           detail: "Logs appear here when Herald processes messages, runs tools, or executes commands.")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(entries) { entry in
                            logEntryRow(entry)
                        }
                    }
                }
            }
        }
    }

    private var logFilterBar: some View {
        HStack(spacing: Design.Spacing.xs) {
            // "All" toggle
            filterChip(
                label: "ALL",
                color: Design.Colors.foreground,
                isActive: selectedLogLevel == nil
            ) { selectedLogLevel = nil }

            ForEach(LogLevel.allCases, id: \.self) { level in
                filterChip(
                    label: level.label,
                    color: level.color,
                    isActive: selectedLogLevel == level
                ) { selectedLogLevel = selectedLogLevel == level ? nil : level }
            }
            Spacer()
            Button { chatStore.logEntries.removeAll() } label: {
                Image(systemName: "trash")
                    .font(.system(size: 11))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, Design.Spacing.xs)
        .overlay(alignment: .bottom) {
            Divider().background(Design.Colors.divider)
        }
    }

    private func filterChip(
        label: String, color: Color, isActive: Bool, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Text(label)
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundStyle(isActive ? color : Design.Colors.secondaryForeground)
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(isActive ? color.opacity(0.12) : Color.clear)
                .clipShape(Capsule())
                .overlay(
                    Capsule().stroke(
                        isActive ? color.opacity(0.3) : Design.Colors.secondaryForeground.opacity(0.2),
                        lineWidth: 1
                    )
                )
        }
        .buttonStyle(.plain)
    }

    private func logEntryRow(_ entry: LogEntry) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Text(entry.timestamp, style: .time)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
            Circle().fill(entry.level.color).frame(width: 6, height: 6).padding(.top, 4)
            Text(entry.message)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(entry.level.color)
                .textSelection(.enabled)
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, 3)
        .background(entry.id == chatStore.logEntries.last?.id ? Design.Brand.accent.opacity(0.06) : Color.clear)
    }

    // MARK: - Terminal (Preview — not yet functional)

    private var terminalContent: some View {
        VStack(spacing: 0) {
            // Preview banner
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 10))
                    .foregroundStyle(.orange)
                Text("Preview — Terminal integration is not yet active")
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(.orange)
                Spacer()
            }
            .padding(.horizontal, Design.Spacing.sm)
            .padding(.vertical, 6)
            .background(Color.orange.opacity(0.08))
            .overlay(alignment: .bottom) {
                Divider().background(Design.Colors.divider)
            }

            VStack(spacing: Design.Spacing.md) {
                Spacer()
                Image(systemName: "apple.terminal")
                    .font(.system(size: 32))
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.4))
                Text("Terminal Coming Soon")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                Text("Interactive terminal access to your Herald host will be available in a future update.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, Design.Spacing.xl)
                Spacer()
            }
            .frame(maxWidth: .infinity)
        }
        .background(Design.Colors.background)
    }

    // MARK: - Usage

    private var toolsContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("TOKEN USAGE")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Design.Colors.secondaryForeground)
                .padding(.horizontal, Design.Spacing.sm)
                .padding(.vertical, Design.Spacing.xs)
            Divider().background(Design.Colors.divider)

            if chatStore.conversation?.latestUsage == nil {
                emptyState(icon: "chart.bar", message: "No usage data yet",
                           detail: "Token usage from the latest Herald response appears here.")
            } else if let usage = chatStore.conversation?.latestUsage {
                VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                    usageRow("Prompt tokens", value: usage.promptTokens)
                    usageRow("Completion tokens", value: usage.completionTokens)
                    usageRow("Total tokens", value: usage.totalTokens)
                }
                .padding(Design.Spacing.sm)
            }
        }
    }

    private func usageRow(_ label: String, value: Int?) -> some View {
        HStack {
            Text(label).font(.system(size: 11, design: .monospaced)).foregroundStyle(Design.Colors.secondaryForeground)
            Spacer()
            Text(value.map { "\($0)" } ?? "--")
                .font(.system(size: 11, weight: .medium, design: .monospaced))
                .foregroundStyle(Design.Colors.foreground)
        }
    }

    private func emptyState(icon: String, message: String, detail: String) -> some View {
        VStack(spacing: Design.Spacing.md) {
            Spacer()
            Image(systemName: icon)
                .font(.system(size: 28))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
            Text(message)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text(detail)
                .font(Design.Typography.caption2)
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                .multilineTextAlignment(.center)
                .padding(.horizontal, Design.Spacing.lg)
            Spacer()
        }
    }
}

// MARK: - Right Panel Tab

enum RightPanelTab: String, CaseIterable, Identifiable {
    case logs, terminal, tools, canvas
    var id: String { rawValue }

    var title: String {
        switch self {
        case .logs: "Activity"
        case .terminal: "Terminal"
        case .tools: "Usage"
        case .canvas: "Canvas"
        }
    }

    var icon: String {
        switch self {
        case .logs: "list.bullet.rectangle"
        case .terminal: "apple.terminal"
        case .tools: "chart.bar"
        case .canvas: "rectangle.on.rectangle"
        }
    }
}

// MARK: - Log Entry

enum LogLevel: String, CaseIterable {
    case info, warn, error, debug, tool

    var label: String {
        switch self {
        case .info:  "INFO"
        case .warn:  "WARN"
        case .error: "ERR"
        case .debug: "DBG"
        case .tool:  "TOOL"
        }
    }

    @MainActor
    var color: Color {
        switch self {
        case .info:  Design.Colors.foreground
        case .warn:  .orange
        case .error: .red
        case .debug: Design.Colors.secondaryForeground
        case .tool:  Design.Brand.accent
        }
    }
}

struct LogEntry: Identifiable {
    let id = UUID()
    let timestamp = Date()
    let level: LogLevel
    let message: String
}
