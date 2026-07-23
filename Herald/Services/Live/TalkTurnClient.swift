import Foundation
import os

@MainActor
final class TalkTurnClient {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "TalkTurn")
    private let heraldClient: any HeraldClientProtocol

    init(heraldClient: any HeraldClientProtocol) {
        self.heraldClient = heraldClient
    }

    /// Submit a user utterance to Hermes and stream the response.
    /// Returns the final canonical text.
    func submitUtterance(
        _ text: String,
        conversationId: UUID,
        clientMessageId: UUID
    ) -> AsyncThrowingStream<TalkTurnUpdate, Error> {
        AsyncThrowingStream { continuation in
            Task {
                let stream = self.heraldClient.sendStreaming(
                    message: text,
                    attachments: [],
                    clientMessageID: clientMessageId
                )

                var canonicalText = ""

                for await update in stream {
                    guard !Task.isCancelled else {
                        continuation.yield(.cancelled)
                        continuation.finish()
                        return
                    }
                    switch update {
                    case .messageSent(let jobID):
                        continuation.yield(.jobAccepted(jobID: jobID))
                    case .textDelta(let delta):
                        canonicalText += delta
                        continuation.yield(.textDelta(delta))
                    case .reasoningDelta:
                        break
                    case .toolActivity(let label):
                        continuation.yield(.toolActivity(label))
                    case .finished(let message, _, _):
                        canonicalText = message.content
                        continuation.yield(.completed(text: canonicalText))
                        continuation.finish()
                        return
                    case .failed(let error, _, _):
                        continuation.finish(throwing: TalkTurnError.hermesError(error))
                        return
                    case .cancelled:
                        continuation.yield(.cancelled)
                        continuation.finish()
                        return
                    default:
                        break
                    }
                }
                // Stream ended without terminal event
                if !canonicalText.isEmpty {
                    continuation.yield(.completed(text: canonicalText))
                }
                continuation.finish()
            }
        }
    }
}

enum TalkTurnUpdate: Sendable {
    case jobAccepted(jobID: UUID)
    case textDelta(String)
    case toolActivity(String)
    case completed(text: String)
    case cancelled
}

enum TalkTurnError: Error, LocalizedError {
    case hermesError(String)

    var errorDescription: String? {
        switch self {
        case .hermesError(let msg):
            "Hermes error: \(msg)"
        }
    }
}
