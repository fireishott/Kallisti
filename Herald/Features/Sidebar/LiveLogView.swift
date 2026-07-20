import SwiftUI

/// Displays live log entries from the Herald dashboard (`:9119`).
///
/// Shows a scrollable list of log entries with level filtering and
/// reconnection status. Designed to be used in the iPad 3-pane layout
/// as the detail column.
struct LiveLogView: View {
    @Environment(DashboardLogService.self) private var logService
    @State private var selectedLevel: LogLevel?
    @State private var autoScroll = true

    var body: some View {
        VStack(spacing: 0) {
            headerBar
            filterBar
            Divider()

            if logService.logLines.isEmpty {
                emptyState
            } else {
                logList
            }

            statusBar
        }
        .background(Design.Colors.background)
    }

    // MARK: - Header

    private var headerBar: some View {
        HStack {
            Text("Live Logs")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()

            Button {
                logService.clearLogs()
            } label: {
                Image(systemName: "trash")
                    .font(.system(size: 12))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .buttonStyle(.plain)
            .help("Clear log entries")

            Button {
                autoScroll.toggle()
            } label: {
                Image(systemName: autoScroll ? "arrow.down.to.line" : "arrow.down")
                    .font(.system(size: 12))
                    .foregroundStyle(autoScroll ? Design.Brand.accent : Design.Colors.secondaryForeground)
            }
            .buttonStyle(.plain)
            .help(autoScroll ? "Auto-scroll enabled" : "Auto-scroll disabled")
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
        .background(Design.Colors.surface)
    }

    // MARK: - Filter Bar

    private var filterBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Design.Spacing.xs) {
                filterChip(nil, label: "All")
                ForEach(LogLevel.allCases, id: \.self) { level in
                    filterChip(level, label: level.label)
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.xs)
        }
        .background(Design.Colors.surface.opacity(0.5))
    }

    private func filterChip(_ level: LogLevel?, label: String) -> some View {
        Button {
            withAnimation(Design.Motion.standard) {
                selectedLevel = level
            }
        } label: {
            Text(label)
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundStyle(
                    selectedLevel == level ? Design.Brand.accent : Design.Colors.secondaryForeground
                )
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(
                    selectedLevel == level ? Design.Brand.accent.opacity(0.12) : Color.clear
                )
                .clipShape(Capsule())
                .overlay(
                    Capsule().stroke(
                        selectedLevel == level ? Design.Brand.accent.opacity(0.3) : Design.Colors.secondaryForeground.opacity(0.2),
                        lineWidth: 1
                    )
                )
        }
        .buttonStyle(.plain)
    }

    // MARK: - Log List

    private var logList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(filteredLogs) { entry in
                        logRow(entry)
                            .id(entry.id)
                    }
                }
            }
            .onChange(of: logService.logLines.count) { _, _ in
                if autoScroll, let last = filteredLogs.last {
                    withAnimation {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }
        }
    }

    private var filteredLogs: [DashboardLogService.LogLine] {
        if let selectedLevel {
            return logService.logLines.filter { $0.level == selectedLevel }
        }
        return logService.logLines
    }

    private func logRow(_ entry: DashboardLogService.LogLine) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Text(entry.timestamp, style: .time)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.6))
                .frame(width: 50, alignment: .leading)

            Circle()
                .fill(entry.level.color)
                .frame(width: 6, height: 6)
                .padding(.top, 4)

            VStack(alignment: .leading, spacing: 2) {
                if let source = entry.source {
                    Text(source)
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                }

                Text(entry.message)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(entry.level.color)
                    .textSelection(.enabled)
            }

            Spacer(minLength: 0)
        }
        .padding(.horizontal, Design.Spacing.sm)
        .padding(.vertical, 3)
        .background(
            entry.id == logService.logLines.last?.id
                ? Design.Brand.accent.opacity(0.06)
                : Color.clear
        )
    }

    // MARK: - Empty State

    private var emptyState: some View {
        VStack(spacing: Design.Spacing.md) {
            Spacer()

            switch logService.connectionState {
            case .disconnected:
                Image(systemName: "play.circle")
                    .font(.system(size: 32))
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
                Text("Dashboard Disconnected")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                Text("Connect to view live logs from the Herald dashboard.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                    .multilineTextAlignment(.center)

            case .connecting:
                ProgressView()
                    .tint(Design.Brand.accent)
                Text("Connecting to Dashboard...")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)

            case .connected:
                Image(systemName: "checkmark.circle")
                    .font(.system(size: 32))
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
                Text("Connected")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                Text("Waiting for log entries...")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))

            case .reconnecting(let attempt):
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 32))
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
                Text("Reconnecting...")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                Text("Attempt \(attempt) — the dashboard may be restarting.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                    .multilineTextAlignment(.center)

            case .failed(let reason):
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 32))
                    .foregroundStyle(Design.Colors.warning)
                Text("Connection Failed")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                Text(reason)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                    .multilineTextAlignment(.center)
            }

            Spacer()
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, Design.Spacing.lg)
    }

    // MARK: - Status Bar

    private var statusBar: some View {
        HStack(spacing: Design.Spacing.xs) {
            Circle()
                .fill(statusColor)
                .frame(width: 6, height: 6)

            Text(statusText)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground)

            Spacer()

            Text("\(logService.logLines.count) entries")
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.xs)
        .background(Design.Colors.surface)
        .overlay(alignment: .top) {
            Divider().background(Design.Colors.divider)
        }
    }

    private var statusColor: Color {
        switch logService.connectionState {
        case .connected: return .green
        case .connecting, .reconnecting: return .orange
        case .failed: return .red
        case .disconnected: return Design.Colors.secondaryForeground
        }
    }

    private var statusText: String {
        switch logService.connectionState {
        case .connected: return "Connected"
        case .connecting: return "Connecting..."
        case .reconnecting(let attempt): return "Reconnecting (attempt \(attempt))"
        case .failed: return "Failed"
        case .disconnected: return "Disconnected"
        }
    }
}
