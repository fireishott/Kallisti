import SwiftUI

/// Gateway log viewer. Fetches recent logs from the relay's /gw/logs endpoint
/// with optional level filtering and live streaming via SSE. In native
/// (direct-gateway) mode, logs come from `hermes logs <source> -n 200` via
/// the gateway's cli.exec (no relay needed).
struct GatewayLogsScreen: View {
    @Environment(AppContainer.self) private var container
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(\.dismiss) private var dismiss

    @State private var logLines: [LogLine] = []
    @State private var errorMessage: String?
    @State private var isLoading = true
    @State private var selectedLevel: String = "info"
    @State private var selectedSource: String = "hermes-gateway"
    @State private var isLiveStreaming = false
    @State private var streamTask: Task<Void, Never>?
    @State private var searchText = ""
    // Build 32: "view all" raises the cli.exec line cap from 200 to 2000 so
    // the log viewer isn't limited to a tiny tail.
    @State private var viewAllLines = false
    @State private var nativePollTask: Task<Void, Never>?
    @State private var liveIngestedCount = 0
    @State private var streamConnectionError: String?

    private let levels = ["debug", "info", "warning", "error"]
    // Build 107: source picker for selecting which logs to view
    private let sources = [("connector", "Connector"), ("hermes-gateway", "Hermes Gateway"), ("hermes-agent", "Hermes Agent")]

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

                // Build 107: Source filter for selecting which logs to view
                sourcePicker

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
                Toggle("View All", isOn: $viewAllLines)
                    .font(.caption)
                    .tint(Design.Brand.accent)
                    .labelsHidden()
                    .onChange(of: viewAllLines) { _, _ in
                        Task { await fetchLogs() }
                    }
                    .accessibilityLabel("View all log lines")
            }
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
        .onDisappear {
            streamTask?.cancel()
            nativePollTask?.cancel()
        }
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

    // MARK: - Source Picker

    // Build 107: source picker for selecting which logs to view
    private var sourcePicker: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(sources, id: \.0) { source, label in
                    Button {
                        selectedSource = source
                        Task { await fetchLogs() }
                    } label: {
                        Text(label)
                            .font(Design.Typography.caption)
                            .fontWeight(.medium)
                            .foregroundStyle(selectedSource == source ? .white : Design.Colors.secondaryForeground)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(selectedSource == source ? Design.Brand.primary : Design.Colors.surface)
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

        // NATIVE mode: pull logs via the gateway's cli.exec
        // (`hermes logs <source> -n 200`). The relay REST /gw/logs facade
        // rejects native bearer tokens, so this is the only working path.
        if container.nativeGatewayClient != nil,
           let featureClient = container.nativeGatewayClient?.featureClient {
            do {
                // Map the source picker to the CLI log name: connector ->
                // gateway (connector logs land in the gateway log on the
                // host), hermes-gateway -> gateway, hermes-agent -> agent.
                let logName: String
                switch selectedSource {
                case "connector": logName = "gateway"
                case "hermes-agent": logName = "agent"
                default: logName = "gateway"
                }
                let lineCount = viewAllLines ? 2000 : 200
                let raw = try await featureClient.cliExec(
                    argv: ["logs", logName, "-n", String(lineCount), "--level", selectedLevel]
                )
                let now = Date()
                logLines = raw.components(separatedBy: .newlines)
                    .filter { !$0.isEmpty }
                    .map { line in
                        LogLine(
                            timestamp: now,
                            level: selectedLevel,
                            message: line,
                            source: selectedSource
                        )
                    }
                return
            } catch {
                errorMessage = "Native log fetch failed: \(error.localizedDescription)"
                return
            }
        }

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
            // Build 107: include source parameter to select which logs to fetch
            let response: Response = try await client.get(
                path: "gw/logs?lines=200&level=\(selectedLevel)&source=\(selectedSource)",
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
            nativePollTask?.cancel()
            nativePollTask = nil
            isLiveStreaming = false
            streamConnectionError = nil
        } else {
            isLiveStreaming = true
            liveIngestedCount = 0
            streamConnectionError = nil
            // NATIVE mode has no relay SSE stream to subscribe to - the relay
            // endpoints reject native bearer tokens (401). Live mode polls the
            // gateway's `hermes logs` via cli.exec on a short timer instead.
            if container.nativeGatewayClient != nil,
               let featureClient = container.nativeGatewayClient?.featureClient {
                startNativeLivePolling(featureClient)
            } else {
                startLiveStream()
            }
        }
    }

    /// Native live: poll `hermes logs` every 3s and append only NEW lines so
    /// the tail grows like a real live stream without resetting the list.
    private func startNativeLivePolling(_ featureClient: NativeGatewayFeatureClient) {
        nativePollTask = Task {
            var lastLineCount = 0
            while !Task.isCancelled {
                let logName: String
                switch self.selectedSource {
                case "connector": logName = "gateway"
                case "hermes-agent": logName = "agent"
                default: logName = "gateway"
                }
                let lineCount = self.viewAllLines ? 2000 : 200
                let now = Date()
                do {
                    let raw = try await featureClient.cliExec(
                        argv: ["logs", logName, "-n", String(lineCount), "--level", self.selectedLevel]
                    )
                    let freshLines = raw.components(separatedBy: .newlines)
                        .filter { !$0.isEmpty }
                        .map { line in
                            LogLine(timestamp: now, level: self.selectedLevel, message: line, source: self.selectedSource)
                        }
                    await MainActor.run {
                        if freshLines.count > lastLineCount {
                            let newOnes = Array(freshLines.suffix(freshLines.count - lastLineCount))
                            self.logLines.append(contentsOf: newOnes)
                            self.liveIngestedCount += newOnes.count
                            if self.logLines.count > 2000 {
                                self.logLines.removeFirst(self.logLines.count - 2000)
                            }
                        }
                        lastLineCount = freshLines.count
                    }
                } catch {
                    if !Task.isCancelled {
                        await MainActor.run {
                            self.streamConnectionError = error.localizedDescription
                        }
                    }
                }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
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

            // Build 107: include source parameter to select which logs to stream
            let stream = client.streamEvents(
                path: "gw/logs/stream?level=\(selectedLevel)&source=\(selectedSource)",
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
