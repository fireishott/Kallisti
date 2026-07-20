import Foundation

enum StreamingUpdate: Sendable {
    case messageSent(jobID: UUID)
    case textDelta(String)
    case reasoningDelta(String)
    case toolActivity(String)
    case keepalive
    case finished(Message, TokenUsage?, CodeDiff?)
    case failed(String)
}
