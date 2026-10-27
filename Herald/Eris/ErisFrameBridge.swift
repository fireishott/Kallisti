import Foundation

// MARK: - Frames the daemon already sends and this client dropped
//
// `ErisFrame.decode` knows four frame names: `delta`, `done`, `error`,
// `cancelled`. The daemon's `FrameKind` has six, and has had since the device
// surface landed:
//
//     delta, reasoning, tool, done, error, cancelled
//     (crates/eris-host/src/device/turns.rs, FrameKind::ALL)
//
// The two this file adds are the two that were falling into
// `default: return nil` and being skipped. That is the whole reason a thought
// card only appears after the answer: the live reasoning frames were being
// discarded, so the only reasoning the UI ever saw came from the transcript
// refetch, which happens once the turn is over.
//
// This is deliberately a second entry point rather than a change to
// `ErisFrame.decode`. Nothing that already compiles changes behaviour until the
// call site in `ErisClient.streamEvents` is switched over, so the switch is a
// one-line change with a one-line revert.
//
// Shape of the daemon's payloads, straight from `device/turns.rs`:
//
//     reasoning: { "text": "..." }
//     tool:      { "name": "...", "arguments": "...", "output": "...",
//                  "is_error": false }
//
// Both are JSON objects in the SSE `data:` field with the kind in `event:`.

/// One fragment of the model's own reasoning.
///
/// Not the answer, and never fed back into a later prompt. It is what the
/// dimmed "thinking" region renders while the answer is still arriving.
struct ErisFrameReasoning: Equatable, Sendable {
    /// This fragment of reasoning text. May be a partial word: the provider
    /// streams reasoning in whatever chunks it likes.
    let text: String
}

/// One tool call the turn ran.
///
/// The daemon publishes this **once, on completion**, carrying the arguments it
/// was called with and what it returned. There is no separate start event, so a
/// client that wants a live "running" row has to open one when the frame lands
/// and close it in the same breath. `ToolActivity` is built here already
/// finished for exactly that reason, and the call site appends it rather than
/// mutating an existing row.
struct ErisFrameTool: Equatable, Sendable {
    /// The tool's name, e.g. `run_command`.
    let name: String
    /// The arguments, as JSON text.
    let arguments: String
    /// What the tool returned.
    let output: String
    /// Whether the tool reported a failure.
    let isError: Bool
    /// The UI row for this call, ready to append to a message's activities.
    ///
    /// `isActive` is false and `finishedAt` is set because the daemon sends this
    /// frame after the call is done. `liveOutput` carries the output so the run
    /// log has something to show immediately; the transcript refetch may replace
    /// it later with the same bytes.
    var activity: ToolActivity {
        ToolActivity(
            label: name,
            isActive: false,
            name: name,
            argsPreview: arguments.isEmpty ? nil : arguments,
            resultPreview: output.isEmpty ? nil : output,
            liveOutput: output,
            finishedAt: .now,
            isError: isError
        )
    }
}

/// Decode the frame vocabulary the daemon actually speaks.
///
/// Returns `ErisFrameOutcome`, never `nil`: the two kinds `ErisFrame` does not
/// model come back as `.reasoning` and `.tool`, the four it does come back
/// wrapped in `.existing`, and a name this client does not know is `.ignored` -
/// the same "skip a frame from a newer daemon" behaviour the old decoder had,
/// minus the optional, so a call site has one thing to switch on instead of two.
///
/// Throwing on malformed JSON for a *known* name is intentional and matches
/// `ErisFrame.decode`: a `reasoning` frame whose data is not an object is a
/// broken stream, not a frame to shrug at.
enum ErisFrameBridge {
    /// Decode one dispatched SSE event, including the two kinds `ErisFrame`
    /// does not model.
    static func decode(event: String?, data: String) throws -> ErisFrameOutcome {
        switch event {
        case "reasoning":
            let object = try ErisJSON.object(data)
            return .reasoning(ErisFrameReasoning(text: object["text"] as? String ?? ""))

        case "asked":
            // Not terminal, and deliberately so: a question arrives mid-turn and
            // the turn keeps running after the answer. Treating it as a terminal
            // frame would end the stream on the very frame that needs a reply.
            let object = try ErisJSON.object(data)
            guard let id = object["id"] as? String else {
                // The second `asked` frame a job sees carries the decision rather
                // than the question. It is the log's acknowledgement, not news, so
                // it decodes to nothing rather than to an empty card.
                return .ignored
            }
            return .asked(try ErisAsked.decode(id: id, request: object["request"]))

        case "tool":
            let object = try ErisJSON.object(data)
            let name = object["name"] as? String ?? ""
            // `arguments` arrives either as a JSON string or as a nested object,
            // depending on the provider seam. Pinning the text once here keeps
            // every consumer from re-deciding what "arguments" means.
            let arguments = text(of: object["arguments"])
            return .tool(
                ErisFrameTool(
                    name: name,
                    arguments: arguments,
                    output: object["output"] as? String ?? "",
                    isError: object["is_error"] as? Bool ?? false
                )
            )

        case "round":
            // The round about to run, sent before the model call. It is the only
            // signal that separates "the model is thinking" from "a command is
            // running": without it both are the word "streaming" and the operator
            // cannot tell work from a hang.
            let object = try ErisJSON.object(data)
            return .round(
                ErisFrameRound(
                    round: object["round"] as? Int ?? 0,
                    maxRounds: object["max_rounds"] as? Int ?? 0,
                    toolsAllowed: object["tools_allowed"] as? Bool ?? true
                )
            )

        case "tool_start":
            // The call about to run. The finished `tool` frame is a receipt that
            // lands after the fact, so a surface that only had that one could say
            // what had already happened and never what is happening.
            let object = try ErisJSON.object(data)
            return .toolStart(
                ErisFrameToolStart(
                    name: object["name"] as? String ?? "",
                    arguments: text(of: object["arguments"])
                )
            )

        case "tool_output":
            // What the running command printed, as it printed it. Live only: the
            // daemon does not journal these, and a reconnect reads the finished
            // `tool` frame that carries the whole output instead.
            let object = try ErisJSON.object(data)
            return .toolOutput(
                ErisFrameToolOutput(
                    name: object["name"] as? String ?? "",
                    chunk: object["chunk"] as? String ?? ""
                )
            )

        case .some(let known):
            // Everything else is the existing decoder's business, including its
            // `nil` for a name neither of us knows, which is now `.ignored` -
            // the same "skip it, do not fail the stream" outcome a missing
            // `event:` produces a line earlier.
            return try ErisFrame.decode(event: known, data: data)
                .map(ErisFrameOutcome.existing) ?? .ignored

        case .none:
            return .ignored
        }
    }

    /// One JSON value as display text, preserving an already-encoded object.
    private static func text(of value: Any?) -> String {
        switch value {
        case nil, is NSNull:
            return ""
        case let already as String:
            return already
        case let object as [String: Any]:
            // Re-encoded rather than `String(describing:)`: the run log renders
            // this, and a Swift dictionary literal's description is neither
            // valid JSON nor stable between runs.
            guard
                let data = try? JSONSerialization.data(
                    withJSONObject: object,
                    options: [.sortedKeys]
                )
            else { return "" }
            return String(decoding: data, as: UTF8.self)
        default:
            return ""
        }
    }
}

/// What one SSE event turned out to be.
enum ErisFrameOutcome: Equatable, Sendable {
    /// One of the two frames the current `ErisFrame` does not model.
    case reasoning(ErisFrameReasoning)
    case tool(ErisFrameTool)
    /// A turn parked on a question. Mid-turn news, never terminal.
    case asked(ErisAsked)
    /// A round is starting, before the model has said anything.
    case round(ErisFrameRound)
    /// A tool call is about to run. The receipt lands later as `.tool`.
    case toolStart(ErisFrameToolStart)
    /// Output from a call that is still running. Live only.
    case toolOutput(ErisFrameToolOutput)
    /// One of the four the current `ErisFrame` already models.
    case existing(ErisFrame)
    /// A name this client does not know. Skip it; do not fail the stream.
    case ignored
}

/// The round about to run: which one, out of how many, and whether it may still
/// call tools.
struct ErisFrameRound: Equatable, Sendable {
    /// One-based round number.
    let round: Int
    /// The turn's tool budget.
    let maxRounds: Int
    /// Whether this round may still call tools.
    let toolsAllowed: Bool

    /// What a surface shows while the model is thinking.
    ///
    /// The closing round says so plainly: a turn that cannot call tools any more
    /// is about to answer, and calling it another "thinking" round would make the
    /// budget look spent twice.
    var phaseLabel: String {
        guard maxRounds > 0 else { return "Thinking" }
        if !toolsAllowed {
            return "Writing the answer (round \(round) of \(maxRounds))"
        }
        return "Thinking (round \(round) of \(maxRounds))"
    }
}

/// A tool call that is about to run.
struct ErisFrameToolStart: Equatable, Sendable {
    /// The tool name.
    let name: String
    /// The arguments, as JSON.
    let arguments: String
}

/// A chunk of output from a call that is still running.
struct ErisFrameToolOutput: Equatable, Sendable {
    /// The tool that is writing.
    let name: String
    /// What it wrote, this chunk.
    let chunk: String
}

/// Whether an outcome ends the stream.
///
/// Only the four frames `ErisFrame` models can be terminal: a reasoning fragment,
/// a finished tool call, a round announcement and a chunk of a running command's
/// output are all mid-turn news. `ignored` never is, or an unrecognised frame
/// from a newer daemon would truncate a stream that is fine.
extension ErisFrameOutcome {
    var isTerminal: Bool {
        switch self {
        case .reasoning, .tool, .asked, .ignored, .round, .toolStart, .toolOutput:
            return false
        case .existing(let frame): return frame.isTerminal
        }
    }
}
