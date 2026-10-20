import Foundation
import Observation

// MARK: - Rendered rows
//
// The Eris `FrameKind` has exactly four names (`delta`, `done`, `error`,
// `cancelled`), so a turn's reasoning and tool output can only reach this app as
// `delta` text. This file renders that text in three distinct ways:
//
//   * plain prose            -> the answer row
//   * a ```thinking fence    -> a collapsed reasoning row
//   * a ```tool fence        -> a compact run log
//
// Fenced regions are the only thing that can move text out of the answer row.
// A reply with no fences renders entirely as answer text, which is the common
// case and the one a user sees on a plain turn.

/// One row on the Eris chat surface.
struct ErisChatItem: Identifiable, Equatable, Sendable {
    /// What the row is, which decides how it renders.
    enum Kind: Equatable, Sendable {
        /// A question the user asked.
        case question
        /// Assistant prose that streamed from `delta` frames.
        case answer
        /// Reasoning text, rendered collapsed and de-emphasised.
        case reasoning
        /// Tool / action output, rendered as a compact run log.
        case toolRun
        /// A typed failure from the daemon.
        case failure
        /// A lifecycle notice (cancelled, stopped).
        case notice
    }

    /// Stable identity for `ForEach`.
    let id: UUID
    /// What the row is.
    var kind: Kind
    /// The text accumulated so far.
    var text: String
    /// Whether text is still arriving into this row.
    var isStreaming: Bool

    init(
        id: UUID = UUID(),
        kind: Kind,
        text: String = "",
        isStreaming: Bool = false
    ) {
        self.id = id
        self.kind = kind
        self.text = text
        self.isStreaming = isStreaming
    }
}

// MARK: - Delta classifier

/// Splits streamed `delta` text into answer, reasoning and tool-run regions.
///
/// The classifier is incremental on purpose: a SSE frame can end mid-line, so it
/// holds the unterminated tail until the rest of the line arrives. Only complete
/// lines are inspected, which is what keeps a fence marker that straddles two
/// frames from being missed.
struct ErisDeltaClassifier {
    /// One classified fragment of streamed text.
    enum Emit: Equatable {
        case answer(String)
        case reasoning(String)
        case toolRun(String)
    }

    private enum Region {
        case prose
        case reasoning
        case toolRun
    }

    private var region: Region = .prose
    private var partialLine = ""

    /// Feed a chunk of delta text and take whatever fragments are now complete.
    mutating func consume(_ delta: String) -> [Emit] {
        guard !delta.isEmpty else { return [] }
        partialLine += delta
        var emits: [Emit] = []
        while let newline = partialLine.firstIndex(of: "\n") {
            let line = String(partialLine[partialLine.startIndex..<newline])
            partialLine = String(partialLine[partialLine.index(after: newline)...])
            if let emit = consume(line: line) {
                emits.append(emit)
            }
        }
        return emits
    }

    /// Take the tail of a turn that ended without a trailing newline.
    mutating func flush() -> [Emit] {
        defer {
            partialLine = ""
            region = .prose
        }
        guard !partialLine.isEmpty else { return [] }
        let line = partialLine
        switch region {
        case .prose: return [.answer(line)]
        case .reasoning: return [.reasoning(line)]
        case .toolRun: return [.toolRun(line)]
        }
    }

    /// Classify one complete line. `nil` means the line was a fence marker.
    private mutating func consume(line: String) -> Emit? {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("```") {
            let info = trimmed
                .dropFirst(3)
                .trimmingCharacters(in: .whitespaces)
                .lowercased()
            if region == .prose {
                if Self.isReasoningFence(info) {
                    region = .reasoning
                    return nil
                }
                if Self.isToolFence(info) {
                    region = .toolRun
                    return nil
                }
                // An ordinary code fence (` ```swift `) is answer text, and the
                // region stays prose so its contents stay answer text too.
                return .answer(line + "\n")
            }
            // Any fence line closes whatever region was open.
            region = .prose
            return nil
        }
        switch region {
        case .prose: return .answer(line + "\n")
        case .reasoning: return .reasoning(line + "\n")
        case .toolRun: return .toolRun(line + "\n")
        }
    }

    private static func isReasoningFence(_ info: String) -> Bool {
        info.hasPrefix("thinking") || info.hasPrefix("reasoning") || info == "think"
    }

    private static func isToolFence(_ info: String) -> Bool {
        info.hasPrefix("tool") || info == "action" || info == "tool_call"
    }
}

// MARK: - Chat view model

/// Drives one Eris conversation: queue a turn, stream its frames, render them.
///
/// `@MainActor` because it renders, and every call into `ErisClient` is awaited
/// so the actor does the network work off the main actor. Subscribe-to-stream is
/// a suspension point, not a block: the loop yields between frames.
@MainActor
@Observable
final class ErisChatViewModel {
    /// Rows in send order.
    private(set) var items: [ErisChatItem] = []
    /// Whether a turn is currently in flight.
    private(set) var isStreaming = false
    /// One-line status: a failure or a lifecycle notice.
    private(set) var banner: String?
    /// Token accounting from the last `done` frame, when the daemon reported it.
    private(set) var usageLine: String?
    /// Whether the user has asked to stop the turn in flight.
    private(set) var isStopping = false

    /// The transport. An actor, so nothing here blocks the main thread.
    private let client: ErisClient
    /// Reports the session a turn landed in, so the daemon store can remember it.
    private let onSessionID: (@MainActor (String) -> Void)?

    /// The job id of the turn in flight, for `cancel`.
    private var currentTurnID: String?
    /// Whether any `delta` frame arrived for the current turn.
    private var receivedDelta = false
    /// Region state for the current turn's streamed text.
    private var classifier = ErisDeltaClassifier()

    init(client: ErisClient, onSessionID: (@MainActor (String) -> Void)? = nil) {
        self.client = client
        self.onSessionID = onSessionID
    }

    /// Whether the send control should be enabled for this draft.
    func canSend(_ text: String) -> Bool {
        !isStreaming && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    // MARK: Send

    /// Queue one turn and consume its frames until it terminates.
    ///
    /// Returns when the turn is over, the stream breaks, or the task is
    /// cancelled; it never throws, so a view can call it from a button action.
    func send(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !isStreaming else { return }

        banner = nil
        usageLine = nil
        receivedDelta = false
        isStopping = false
        classifier = ErisDeltaClassifier()
        items.append(ErisChatItem(kind: .question, text: trimmed))
        isStreaming = true

        defer {
            isStreaming = false
            isStopping = false
            currentTurnID = nil
            clearStreamingFlags()
        }

        do {
            let turn = try await client.createTurn(text: trimmed)
            currentTurnID = turn.jobID
            onSessionID?(turn.sessionID)
            try await consume(turnID: turn.jobID)
        } catch {
            record(error)
        }
    }

    /// Ask the daemon to cancel the turn in flight.
    ///
    /// The stream is what ends the turn: the daemon answers the next event with
    /// a `cancelled` frame. A `409 turn_not_cancellable` means the turn finished
    /// between the tap and this call, which is not a failure worth showing.
    func stop() async {
        guard let turnID = currentTurnID, isStreaming else { return }
        isStopping = true
        do {
            try await client.cancel(turnID: turnID)
        } catch let error as ErisError {
            if error.code == "turn_not_cancellable" { return }
            banner = error.errorDescription
        } catch {
            banner = error.localizedDescription
        }
    }

    // MARK: Stream

    private func consume(turnID: String) async throws {
        let stream = await client.events(turnID: turnID)
        for try await frame in stream {
            apply(frame)
            if frame.isTerminal { break }
        }
    }

    private func apply(_ frame: ErisFrame) {
        switch frame {
        case .delta(let text):
            guard !text.isEmpty else { return }
            receivedDelta = true
            for emit in classifier.consume(text) {
                append(emit)
            }
        case .done(let text, let usage):
            for emit in classifier.flush() {
                append(emit)
            }
            // A daemon that reports only the assembled reply still gets its text
            // on screen, rather than an empty answer row.
            if !receivedDelta, !text.isEmpty {
                append(.answer(text))
            }
            usageLine = usage.flatMap(Self.usageText)
            clearStreamingFlags()
        case .error(let error):
            banner = error.errorDescription
            items.append(
                ErisChatItem(kind: .failure, text: Self.detail(for: error))
            )
            clearStreamingFlags()
        case .cancelled(let reason):
            banner = reason.map { "turn cancelled (\($0))" } ?? "turn cancelled"
            items.append(ErisChatItem(kind: .notice, text: "Stopped."))
            clearStreamingFlags()
        }
    }

    private func append(_ emit: ErisDeltaClassifier.Emit) {
        switch emit {
        case .answer(let text):
            append(text, kind: .answer)
        case .reasoning(let text):
            append(text, kind: .reasoning)
        case .toolRun(let text):
            append(text, kind: .toolRun)
        }
    }

    /// Append to the open row of the same kind, or start a new one.
    private func append(_ text: String, kind: ErisChatItem.Kind) {
        guard !text.isEmpty else { return }
        if let last = items.indices.last, items[last].kind == kind {
            items[last].text += text
            items[last].isStreaming = true
            return
        }
        items.append(ErisChatItem(kind: kind, text: text, isStreaming: true))
    }

    private func clearStreamingFlags() {
        for index in items.indices where items[index].isStreaming {
            items[index].isStreaming = false
        }
    }

    private func record(_ error: any Error) {
        if error is CancellationError {
            banner = "the turn was cancelled"
            return
        }
        guard let erisError = error as? ErisError else {
            banner = error.localizedDescription
            items.append(ErisChatItem(kind: .failure, text: error.localizedDescription))
            return
        }
        banner = erisError.errorDescription
        items.append(ErisChatItem(kind: .failure, text: Self.detail(for: erisError)))
    }

    // MARK: Text

    /// The message a user reads for one typed failure, with its stable code.
    private static func detail(for error: ErisError) -> String {
        let message = error.errorDescription ?? "Eris failed"
        guard let code = error.code, !code.isEmpty else { return message }
        return "\(message)\ncode: \(code)"
    }

    /// One-line token accounting from a `done` frame.
    private static func usageText(_ usage: ErisUsage) -> String? {
        var parts: [String] = []
        if let input = usage.inputTokens { parts.append("in \(input)") }
        if let output = usage.outputTokens { parts.append("out \(output)") }
        if let cached = usage.cacheReadTokens { parts.append("cached \(cached)") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}
