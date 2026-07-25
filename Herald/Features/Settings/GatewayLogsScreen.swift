import SwiftUI

/// Gateway log viewer. Fetches recent logs from the relay's /gw/logs endpoint
/// with optional level filtering and live streaming via SSE.
struct GatewayLogsScreen: View {
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(\.dismiss) private var dismiss

    @State private var logLines: [LogLine] = []
    @State private var errorMessage: String?
    @State private var isLoading = true
    @State private var selectedLevel: String = "info"
    @State private var isLiveStreaming = false
    @State private var streamTask: Task<Void, Never>?
    @State private var searchText = ""
    @State private var liveIngestedCount = 0
    @State private var streamConnectionError: String?

    private let levels = ["debug", "info", "warning", "error"]

    private var filteredLogs: [LogLine] {
        if searchText.isEmpty { return logLines }
        return logLines.filter { $0.message.localizedCaseInsensitiveContains(searchText) }
    }

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            VStack(spacing: 0) {
                // Level filter
                levelPicker

                if isLoading && logLines.isEmpty {
                    Spacer()
                    ProgressView("Loading logs…")
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Spacer()
                } else if let error = errorMessage, logLines.isEmpty {
                    Spacer()
                    VStack(spacing: Design.Spacing.md) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.system(size: 40))
                            .foregroundStyle(Design.Colors.warning)
                        Text(error)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                        Button("Retry") {
                            Task { await fetchLogs() }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                    Spacer()
                } else if logLines.isEmpty && !isLoading {
                    emptyStateView
                } else {
                    logList
                }
            }
        }
        .navigationTitle("Gateway Logs")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    toggleLiveStream()
                } label: {
                    HStack(spacing: 4) {
                        Circle()
                            .fill(statusDotColor)
                            .frame(width: 6, height: 6)
                        Text(statusLabel)
                            .font(Design.Typography.caption)
                    }
                }
                .foregroundStyle(Design.Colors.foreground)
            }
        }
        .task { await fetchLogs() }
        .onDisappear { streamTask?.cancel() }
    }

    // MARK: - Level Picker

    private var levelPicker: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(levels, id: \.self) { level in
                    Button {
                        selectedLevel = level
                        Task { await fetchLogs() }
                    } label: {
                        Text(level.uppercased())
                            .font(Design.Typography.caption)
                            .fontWeight(.medium)
                            .foregroundStyle(selectedLevel == level ? .white : Design.Colors.secondaryForeground)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(selectedLevel == level ? levelColor(level) : Design.Colors.surface)
                            .clipShape(Capsule())
                    }
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.sm)
        }
        .background(Design.Colors.background)
    }

    // MARK: - Log List

    private var logList: some View {
        List {
            ForEach(filteredLogs.indices, id: \.self) { i in
                logRow(filteredLogs[i])
                    .listRowBackground(Design.Colors.background)
                    .listRowSeparator(.hidden)
            }
        }
        .listStyle(.plain)
        .searchable(text: $searchText, prompt: "Filter logs…")
        .refreshable { await fetchLogs() }
    }

    private func logRow(_ line: LogLine) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                Circle()
                    .fill(levelColor(line.level))
                    .frame(width: 6, height: 6)

                Text(line.level.uppercased())
                    .font(.caption2.monospaced())
                    .foregroundStyle(levelColor(line.level))

                Spacer()

                Text(line.timestamp, style: .time)
                    .font(.caption2.monospaced())
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }

            Text(line.message)
                .font(.caption.monospaced())
                .foregroundStyle(Design.Colors.foreground)
                .lineLimit(6)
        }
        .padding(.vertical, 4)
    }

    // MARK: - Data

    private func fetchLogs() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString
            guard let relayBase else {
                errorMessage = "No relay configured. Add your relay URL in Settings."
                return
            }
            let token = await sessionStore.currentAccessToken()
            let client = RelayAPIClient { relayBase }

            struct Response: Decodable {
                struct Data: Decodable {
                    let lines: [LogLine]
                }
                let data: Data
            }
            let response: Response = try await client.get(
                path: "gw/logs?lines=200&level=\(selectedLevel)",
                accessToken: token
            )
            logLines = response.data.lines
        } catch let error as RelayAPIClient.ClientError {
            switch error {
            case .serverError(let code, _, _, let status):
                if status == 404 {
                    errorMessage = "The relay does not expose gateway logs. Update your connector to a version that supports the /gw/logs endpoint."
                } else {
                    errorMessage = "[\(code)] \(error.localizedDescription)"
                }
            default:
                errorMessage = error.localizedDescription
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func toggleLiveStream() {
        if isLiveStreaming {
            streamTask?.cancel()
            streamTask = nil
            isLiveStreaming = false
            streamConnectionError = nil
        } else {
            isLiveStreaming = true
            liveIngestedCount = 0
            streamConnectionError = nil
            startLiveStream()
        }
    }

    private func startLiveStream() {
        streamTask = Task {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString
            guard let relayBase else {
                await MainActor.run {
                    errorMessage = "No relay configured. Add your relay URL in Settings."
                    isLiveStreaming = false
                }
                return
            }
            let token = await sessionStore.currentAccessToken()
            let client = RelayAPIClient { relayBase }

            let stream = client.streamEvents(
                path: "gw/logs/stream?level=\(selectedLevel)",
                accessToken: token,
                lastEventID: nil
            )

            do {
                for try await sseEvent in stream {
                    guard !Task.isCancelled else { break }
                    // Parse log line from SSE data
                    if let data = sseEvent.data.data(using: .utf8),
                       let line = try? JSONDecoder().decode(LogLine.self, from: data) {
                        await MainActor.run {
                            logLines.append(line)
                            liveIngestedCount += 1
                            // Keep only last 500 lines
                            if logLines.count > 500 {
                                logLines.removeFirst(logLines.count - 500)
                            }
                        }
                    }
                }
            } catch {
                if !Task.isCancelled {
                    await MainActor.run {
                        isLiveStreaming = false
                        streamConnectionError = error.localizedDescription
                    }
                }
            }
        }
    }

    // MARK: - Status

    private var statusDotColor: Color {
        if streamConnectionError != nil { return Design.Colors.danger }
        if isLiveStreaming { return Design.Colors.success }
        return Design.Colors.secondaryForeground
    }

    private var statusLabel: String {
        if let _ = streamConnectionError {
            return "Error"
        }
        if isLiveStreaming {
            if liveIngestedCount > 0 {
                return "Live (\(liveIngestedCount))"
            }
            return "Live"
        }
        return "Paused"
    }

    // MARK: - Empty State

    private var emptyStateView: some View {
        VStack(spacing: Design.Spacing.md) {
            Spacer()
            Image(systemName: "doc.text.magnifyingglass")
                .font(.system(size: 40))
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.5))
            Text("No Log Entries")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text("The relay returned no log entries for level \"\(selectedLevel)\". Try a different level or toggle Live mode to wait for new entries.")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                .multilineTextAlignment(.center)
                .padding(.horizontal, Design.Spacing.lg)
            if let error = streamConnectionError {
                Text("Stream error: \(error)")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.warning)
            }
            Spacer()
        }
    }

    // MARK: - Helpers

    private func levelColor(_ level: String) -> Color {
        switch level.lowercased() {
        case "error", "critical": return Design.Colors.danger
        case "warning": return Design.Colors.warning
        case "info": return Design.Colors.success
        case "debug": return Design.Colors.secondaryForeground
        default: return Design.Colors.secondaryForeground
        }
    }
}

struct LogLine: Decodable, Identifiable {
    var id: String { "\(timestamp.timeIntervalSince1970)-\(message.hashValue)" }
    let timestamp: Date
    let level: String
    let message: String
    let source: String?
}
