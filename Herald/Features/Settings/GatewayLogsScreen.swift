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
                            .fill(isLiveStreaming ? Design.Colors.success : Design.Colors.secondaryForeground)
                            .frame(width: 6, height: 6)
                        Text(isLiveStreaming ? "Live" : "Paused")
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
            isLiveStreaming = false
        } else {
            isLiveStreaming = true
            startLiveStream()
        }
    }

    private func startLiveStream() {
        streamTask = Task {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString
            guard let relayBase else {
                errorMessage = "No relay configured. Add your relay URL in Settings."
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
                            // Keep only last 500 lines
                            if logLines.count > 500 {
                                logLines.removeFirst(logLines.count - 500)
                            }
                        }
                    }
                }
            } catch {
                if !Task.isCancelled {
                    await MainActor.run { isLiveStreaming = false }
                }
            }
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
