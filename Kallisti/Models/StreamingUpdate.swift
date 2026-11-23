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
    case messageSent(jobID: UUID, daemonJobID: String?)
    case textDelta(String)
    case reasoningDelta(String)
    case toolActivity(String)
    /// A turn.artifact.diff frame from the Eris daemon, in the shape the chat
    /// renders. The card is the same one the relay path used.
    case artifactDiff(CodeDiff)
    /// A turn.artifact.page frame: html a turn wrote, with the daemon height cap.
    case artifactPage(html: String, heightCap: Int?)
    /// A file is going up: `completed` of `total`, this one being `name`.
    case uploading(attachmentID: UUID, completed: Int, total: Int, name: String)
    case toolStarted(ToolActivity)
    case toolCompleted(toolCallID: String, resultPreview: String?, isError: Bool, durationMs: Int?)
    case toolOutput(toolCallID: String, chunk: String)
    /// The turn loop's round announcement: which round is about to run, and the
    /// ceiling it runs against. Drawn live so a long turn says where it is
    /// instead of only that it is working.
    case round(count: Int, total: Int)
    /// A file the turn attached to its own reply, streamed when `attach_file`
    /// succeeds. Without this the bubble being watched had no chip until the
    /// conversation was reloaded.
    case attachment(MessageAttachment)
    /// Build 104: out-of-band Hermes system notice (self-improvement review
    /// summaries, memory updates). Rendered as a faint in-transcript line.
    case systemNotice(String)
    /// Build 128.76: the agent parked the turn on a clarify question. The
    /// payload carries the question, the choice list (nil for open-ended),
    /// and the request_id the answer must ride back on via clarify.respond.
    case clarifyRequest(question: String, choices: [String]?, requestID: String, multiSelect: Bool)
    case started(phase: String)
    case heartbeat(phase: String)
    case reconnecting
    case cancelled
    case keepalive
    case finished(Message, TokenUsage?, CodeDiff?, ContextInfo?)
    case failed(String, category: String? = nil, action: String? = nil)
}
