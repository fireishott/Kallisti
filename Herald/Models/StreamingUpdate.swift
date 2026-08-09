import Foundation

struct ContextInfo: Codable, Hashable, Sendable {
    let window: Int
    let used: Int
    var percentUsed: Double {
        guard window > 0 else { return 0 }
        return min(Double(used) / Double(window) * 100.0, 100.0)
    }
}

enum StreamingUpdate: Sendable {
    case messageSent(jobID: UUID)
    case textDelta(String)
    case reasoningDelta(String)
    case toolActivity(String)
    case toolStarted(ToolActivity)
    case toolCompleted(toolCallID: String, resultPreview: String?, isError: Bool, durationMs: Int?)
    case started(phase: String)
    case heartbeat(phase: String)
    case reconnecting
    case cancelled
    case keepalive
    case finished(Message, TokenUsage?, CodeDiff?, ContextInfo?)
    case failed(String, category: String? = nil, action: String? = nil)
}
