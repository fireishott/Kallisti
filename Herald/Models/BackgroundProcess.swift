import Foundation

/// A tracked background process surfaced by the connector's
/// BackgroundProcessRegistry. Mirrors the wire shape emitted by
/// /v1/canvas/processes/stream (SSE). `Identifiable` by process id.
struct BackgroundProcess: Codable, Identifiable, Hashable, Sendable {
    enum Status: String, Codable, Sendable {
        case starting, running, completed, failed, killed
        var isActive: Bool { self == .starting || self == .running }
    }

    var id: String
    var name: String
    var command: [String]
    var pid: Int?
    var status: Status
    var exitCode: Int?
    var startedAt: Date?
    var finishedAt: Date?
    var stream: String?
    var chunk: String?
    var outputTail: String?

    /// Human-readable one-line command summary for row labels.
    var commandLine: String {
        if !command.isEmpty { return command.joined(separator: " ") }
        return name
    }

    /// Merge an incremental SSE event into this row (chunk + refreshed
    /// status/tail). Hash excludes the live output so streaming doesn't
    /// flicker the entire list: rows only re-render when identity fields
    /// change, while output is handled by the tail view.
    func merged(with event: BackgroundProcess) -> BackgroundProcess {
        var copy = self
        copy.name = event.name.isEmpty ? copy.name : event.name
        if !event.command.isEmpty { copy.command = event.command }
        if let pid = event.pid { copy.pid = pid }
        copy.status = event.status
        if let exitCode = event.exitCode { copy.exitCode = exitCode }
        if let startedAt = event.startedAt { copy.startedAt = startedAt }
        if let finishedAt = event.finishedAt { copy.finishedAt = finishedAt }
        copy.stream = event.stream ?? copy.stream
        copy.chunk = event.chunk ?? copy.chunk
        copy.outputTail = event.outputTail ?? copy.outputTail
        return copy
    }
}