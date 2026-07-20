import Foundation
import os

/// Connects to the Herald dashboard (`:9119`) via SSE and streams log entries.
///
/// The dashboard is a separate process that can wedge or restart. This service
/// handles reconnection with exponential backoff and never crashes the parent
/// view when the connection drops.
@MainActor
@Observable
final class DashboardLogService {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "DashboardLogService")

    struct LogLine: Identifiable, Sendable {
        let id = UUID()
        let timestamp: Date
        let level: LogLevel
        let message: String
        let source: String?
    }

    enum ConnectionState: Sendable {
        case disconnected
        case connecting
        case connected
        case reconnecting(attempt: Int)
        case failed(String)
    }

    var connectionState: ConnectionState = .disconnected
    var logLines: [LogLine] = []
    var isConnected: Bool {
        if case .connected = connectionState { return true }
        return false
    }

    private let baseURLProvider: @MainActor () -> String
    private let credentialsProvider: @MainActor () -> (username: String, password: String)?
    private var streamTask: Task<Void, Never>?
    private var reconnectAttempt = 0
    private static let maxReconnectAttempts = 10
    private static let maxLogLines = 500

    init(
        baseURLProvider: @escaping @MainActor () -> String,
        credentialsProvider: @escaping @MainActor () -> (username: String, password: String)?
    ) {
        self.baseURLProvider = baseURLProvider
        self.credentialsProvider = credentialsProvider
    }

    func connect() {
        guard streamTask == nil else { return }
        connectionState = .connecting
        reconnectAttempt = 0
        streamTask = Task { await runStreamLoop() }
    }

    func disconnect() {
        streamTask?.cancel()
        streamTask = nil
        connectionState = .disconnected
    }

    func clearLogs() {
        logLines.removeAll()
    }

    private func runStreamLoop() async {
        while !Task.isCancelled {
            do {
                try await connectAndStream()
                // If we get here, the stream ended normally — reconnect
                reconnectAttempt = 0
            } catch is CancellationError {
                break
            } catch {
                Self.logger.warning("Dashboard stream error: \(error.localizedDescription)")
            }

            guard !Task.isCancelled else { break }

            // Exponential backoff: 1s, 2s, 4s, 8s, ... capped at 30s
            reconnectAttempt += 1
            if reconnectAttempt > Self.maxReconnectAttempts {
                connectionState = .failed("Max reconnection attempts reached")
                break
            }

            let delay = min(pow(2.0, Double(reconnectAttempt - 1)), 30.0)
            connectionState = .reconnecting(attempt: reconnectAttempt)
            Self.logger.info("Reconnecting to dashboard in \(delay)s (attempt \(self.reconnectAttempt))")

            try? await Task.sleep(for: .seconds(delay))
        }
    }

    private func connectAndStream() async throws {
        let baseURL = baseURLProvider()
        guard let url = URL(string: "\(baseURL)/logs/stream") else {
            throw URLError(.badURL)
        }

        var request = URLRequest(url: url)
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        request.timeoutInterval = TimeInterval(Int.max) // SSE connections are long-lived

        // Add basic auth if credentials are available
        if let creds = credentialsProvider() {
            let authString = "\(creds.username):\(creds.password)"
            if let authData = authString.data(using: .utf8) {
                request.setValue("Basic \(authData.base64EncodedString())", forHTTPHeaderField: "Authorization")
            }
        }

        connectionState = .connected
        reconnectAttempt = 0

        let (bytes, response) = try await URLSession.shared.bytes(for: request)

        guard let httpResponse = response as? HTTPURLResponse,
              (200...299).contains(httpResponse.statusCode) else {
            throw URLError(.badServerResponse)
        }

        var currentEvent = ""
        var currentData = ""

        for try await line in bytes.lines {
            guard !Task.isCancelled else { break }

            if line.isEmpty {
                // Process the event
                if !currentEvent.isEmpty || !currentData.isEmpty {
                    processEvent(event: currentEvent, data: currentData)
                }
                currentEvent = ""
                currentData = ""
                continue
            }

            if line.hasPrefix("event:") {
                currentEvent = String(line.dropFirst(6)).trimmingCharacters(in: .whitespaces)
            } else if line.hasPrefix("data:") {
                let newData = String(line.dropFirst(5)).trimmingCharacters(in: .whitespaces)
                if currentData.isEmpty {
                    currentData = newData
                } else {
                    currentData += "\n" + newData
                }
            } else if line.hasPrefix(":") {
                // Keepalive comment — ignore
                continue
            }
        }
    }

    private func processEvent(event: String, data: String) {
        // Parse the log line from the dashboard
        // Expected format: JSON with timestamp, level, message, source
        guard let jsonData = data.data(using: .utf8) else { return }

        struct DashboardLogEntry: Decodable {
            let timestamp: String?
            let level: String?
            let message: String?
            let source: String?
        }

        if let entry = try? JSONDecoder().decode(DashboardLogEntry.self, from: jsonData) {
            let logLevel: LogLevel
            switch entry.level?.lowercased() {
            case "error", "err": logLevel = .error
            case "warning", "warn": logLevel = .warn
            case "debug", "dbg": logLevel = .debug
            case "tool": logLevel = .tool
            default: logLevel = .info
            }

            let timestamp: Date
            if let ts = entry.timestamp {
                timestamp = ISO8601DateFormatter().date(from: ts) ?? .now
            } else {
                timestamp = .now
            }

            let logLine = LogLine(
                timestamp: timestamp,
                level: logLevel,
                message: entry.message ?? data,
                source: entry.source
            )

            logLines.append(logLine)

            // Trim to max lines
            if logLines.count > Self.maxLogLines {
                logLines.removeFirst(logLines.count - Self.maxLogLines)
            }
        } else {
            // If JSON parsing fails, treat as plain text
            let logLine = LogLine(
                timestamp: .now,
                level: .info,
                message: data,
                source: nil
            )
            logLines.append(logLine)

            if logLines.count > Self.maxLogLines {
                logLines.removeFirst(logLines.count - Self.maxLogLines)
            }
        }
    }
}
