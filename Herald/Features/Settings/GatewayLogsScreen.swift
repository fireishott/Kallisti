import SwiftUI
import os

/// Gateway log viewer. Fetches recent logs from the relay's /gw/logs endpoint
/// with optional level filtering and live streaming via SSE. In native
/// (direct-gateway) mode, logs come from `hermes logs <source> -n 200` via
/// the gateway's cli.exec (no relay needed).
struct GatewayLogsScreen: View {
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "GatewayLogsScreen")
    @Environment(AppContainer.self) private var container
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(\.dismiss) private var dismiss

    @State private var logLines: [LogLine] = []
    @State private var errorMessage: String?
    @State private var isLoading = true
    @State private var selectedLevel: String = "all"
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

    private let levels = ["all", "debug", "info", "warning", "error"]
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

        // NATIVE mode: prefer the connector HTTP facade (/gw/logs on port
        // 8010) with the native bearer token. The facade reads journald
        // directly and returns real timestamps + per-line levels; cli.exec
        // truncates output at 48K chars (cutting the NEWEST lines) and
        // stamps every line with the selected level. Fall back to cli.exec
        // if the facade is unreachable.
        if container.nativeGatewayClient != nil,
           let nativeClient = container.nativeGatewayClient {
            do {
                guard let facadeBase = await nativeClient.facadeBaseURLString(),
                      // Build 55: refresh the bearer first — the facade
                      // verifies against the gateway, and an expired token
                      // 401s every log fetch even when the socket is healthy.
                      let nativeToken = await nativeClient.refreshAccessToken(),
                      let url = URL(string: "\(facadeBase)/gw/logs?lines=\(viewAllLines ? 2000 : 200)&level=\(selectedLevel)&source=\(selectedSource)")
                else {
                    throw NativeGatewayClientError.notConnected
                }
                var request = URLRequest(url: url)
                request.setValue("Bearer \(nativeToken)", forHTTPHeaderField: "Authorization")
                request.timeoutInterval = 15
                let (data, response) = try await URLSession.shared.data(for: request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                    throw NativeGatewayClientError.unexpectedFrame
                }
                struct FacadeResponse: Decodable {
                    struct Data: Decodable { let lines: [LogLine] }
                    let data: Data
                }
                let decoder = JSONDecoder()
                decoder.dateDecodingStrategy = .iso8601
                let decoded = try decoder.decode(FacadeResponse.self, from: data)
                logLines = decoded.data.lines
                return
            } catch {
                // Facade failed - fall through to cli.exec below.
                Self.logger.warning("facade log fetch failed: \(error.localizedDescription)")
            }
        }

        // Fallback: pull logs via the gateway's cli.exec
        // (`hermes logs <source> -n 200`). Only used when the facade path
        // above is unavailable (e.g. no native bearer yet).
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
                let cliLevel = selectedLevel == "all" ? "debug" : selectedLevel
                let raw = try await featureClient.cliExec(
                    argv: ["logs", logName, "-n", String(lineCount), "--level", cliLevel]
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
            // NATIVE mode: stream from the connector HTTP facade
            // (/gw/logs/stream on port 8010) with the native bearer token.
            // The old path polled `hermes logs` via cli.exec every 3s with a
            // count-based diff that broke once the log exceeded the window
            // (count stayed 200 forever, so nothing ever appended) - "Live"
            // looked frozen. The facade's SSE stream is real-time and
            // returns real per-line levels.
            if container.nativeGatewayClient != nil,
               let nativeClient = container.nativeGatewayClient {
                startNativeFacadeStream(nativeClient)
            } else {
                startLiveStream()
            }
        }
    }

    /// Native live: SSE stream from the connector facade /gw/logs/stream
    /// (port 8010). Authenticates with the native gateway bearer token
    /// (dual-auth endpoint, same as /v1/native/watch).
    private func startNativeFacadeStream(_ nativeClient: NativeKallistiClient) {
        streamTask = Task {
            guard let facadeBase = await nativeClient.facadeBaseURLString(),
                  let nativeToken = await nativeClient.nativeAccessToken(),
                  let url = URL(string: "\(facadeBase)/gw/logs/stream?level=\(selectedLevel)&source=\(selectedSource)")
            else {
                await MainActor.run {
                    streamConnectionError = "Native log stream unavailable (no bearer token)"
                    isLiveStreaming = false
                }
                return
            }
            var request = URLRequest(url: url)
            request.setValue("Bearer \(nativeToken)", forHTTPHeaderField: "Authorization")
            request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
            request.timeoutInterval = TimeInterval(Int.max)

            do {
                let (bytes, response) = try await URLSession.shared.bytes(for: request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                    await MainActor.run {
                        streamConnectionError = "Native log stream returned HTTP \((response as? HTTPURLResponse)?.statusCode ?? -1)"
                        isLiveStreaming = false
                    }
                    return
                }
                for try await line in bytes.lines {
                    guard !Task.isCancelled else { break }
                    guard line.hasPrefix("data:") else { continue }
                    let payload = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
                    guard !payload.isEmpty,
                          let data = payload.data(using: .utf8),
                          let logLine = try? JSONDecoder().decode(LogLine.self, from: data)
                    else { continue }
                    await MainActor.run {
                        logLines.append(logLine)
                        liveIngestedCount += 1
                        if logLines.count > 2000 {
                            logLines.removeFirst(logLines.count - 2000)
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
