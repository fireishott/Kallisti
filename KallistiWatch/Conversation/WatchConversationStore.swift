//
//  WatchConversationStore.swift
//  KallistiWatch
//
//  One state, no parallel booleans. The orb is a single enum, so a screen can
//  never show "listening" and "thinking" at the same time, and the tool label
//  that makes a long turn legible is part of the state rather than a side note.
//

import Foundation
import Observation
import WidgetKit

/// What the wrist is doing, in one value.
enum WatchTalkState: Sendable, Equatable {
    case idle
    case connecting
    case listening
    case thinking(tool: String?)
    case speaking
    case interrupted
    case failed(reason: String)

    /// The word the orb shows.
    var word: String {
        switch self {
        case .idle: "idle"
        case .connecting: "connecting"
        case .listening: "listening"
        case let .thinking(tool): tool.map { "thinking: \($0)" } ?? "thinking"
        case .speaking: "speaking"
        case .interrupted: "interrupted"
        case let .failed(reason): "failed: \(reason)"
        }
    }

    /// Whether the mic should be open.
    var listens: Bool {
        self == .listening
    }
}

/// One line of the conversation as the screen draws it.
struct WatchTurn: Identifiable, Equatable, Sendable {
    let id: UUID
    var role: Role
    var text: String

    enum Role: String, Sendable {
        case user
        case assistant
    }
}

/// Drives one conversation: the state, the transcript, and the carrier.
@MainActor
@Observable
final class WatchConversationStore {
    private(set) var state: WatchTalkState = .idle
    private(set) var turns: [WatchTurn] = []
    /// Which transport carried the last turn. On screen, because a wrist that
    /// silently fell back to direct is a wrist whose battery the operator cannot
    /// explain.
    private(set) var carrier: TransportName = .none
    /// One clause of the answer, ready to be heard.
    var onClause: (@MainActor (Data) -> Void)?

    private var jobID: String?
    private var eventsTask: Task<Void, Never>?

    /// Begin: the session comes up before the mic opens. A failed turn is a turn
    /// a person can start again.
    func begin() {
        switch state {
        case .idle, .failed: break
        default: return
        }
        state = .connecting
        state = .listening
    }

    /// The person finished speaking: send what was heard.
    func send(_ text: String, using chooser: WatchTransportChooser) async {
        let clean = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty else {
            fail("empty_transcript")
            return
        }
        turns.append(WatchTurn(id: UUID(), role: .user, text: clean))
        state = .thinking(tool: nil)
        publishToFace()
        do {
            let result = try await chooser.sendTurn(text: clean)
            carrier = result.carrier
            jobID = result.handle
        } catch let error as WatchTransportError {
            fail(error.word)
        } catch let error as WatchAuthError {
            fail(error.word)
        } catch {
            fail("unreachable")
        }
    }

    /// Follow the turn this job names.
    func follow(using transport: any WatchTransport) {
        guard let jobID else { return }
        eventsTask?.cancel()
        eventsTask = Task { [weak self] in
            do {
                for try await frame in transport.events(jobID: jobID, audio: true) {
                    guard let self else { return }
                    self.apply(frame)
                }
            } catch WatchTransportError.framesUnavailable {
                // The relay cannot stream frames yet. The turn is still running
                // on the host; say which path failed rather than pretending.
                self?.fail("\(transport.name.rawValue) \(WatchTransportError.framesUnavailable.word)")
            } catch {
                self?.fail(transport.name.rawValue)
            }
        }
    }

    /// One frame, into the state.
    func apply(_ frame: WatchFrame) {
        switch frame {
        case .start:
            if case .thinking = state { return }
            state = .thinking(tool: nil)
        case let .tool(name):
            // The label is the whole reason this frame exists: a bare orb for
            // forty seconds is the defect people call "broken".
            state = .thinking(tool: name)
        case let .delta(text):
            appendAssistant(text)
            if case .thinking = state {
                state = .speaking
            }
        case let .clause(_, wav, text):
            appendAssistant(text)
            state = .speaking
            onClause?(wav)
        case let .done(text):
            if !text.isEmpty { appendAssistant(text) }
            state = .idle
            jobID = nil
            publishToFace()
        case .failed:
            fail("turn_failed")
        case .cancelled:
            state = .idle
            jobID = nil
        }
    }

    /// A wrist raised away mid-answer keeps the audio; the mic is the thing that
    /// stops. While listening, a lower wrist ends the capture and keeps what was
    /// heard so far.
    func wristLowered() {
        switch state {
        case .speaking:
            return
        case .listening:
            state = .thinking(tool: nil)
        default:
            return
        }
    }

    /// The orb was tapped while the answer was being spoken: the audio stops and
    /// the turn is cut. The mic opens in [`resumeListening`], when the screen has
    /// actually started a capture - a state that says "listening" before the
    /// microphone is open is a lie the next frame would expose.
    func bargeIn() {
        guard state == .speaking else { return }
        state = .interrupted
    }

    /// Open the mic after a barge-in.
    func resumeListening() {
        guard state == .interrupted else { return }
        state = .listening
    }

    /// Nothing left to do.
    func settle() {
        eventsTask?.cancel()
        if case .failed = state { return }
        state = .idle
    }

    private func appendAssistant(_ text: String) {
        if let last = turns.last, last.role == .assistant {
            turns[turns.count - 1].text += text
        } else {
            turns.append(WatchTurn(id: UUID(), role: .assistant, text: text))
        }
    }

    func fail(_ word: String) {
        // The carrier is only worth naming once a transport carried something:
        // "none: turn_failed" tells a person less than the failure does.
        let prefix = carrier == .none ? "" : "\(carrier.rawValue): "
        state = .failed(reason: prefix + word)
        jobID = nil
        publishToFace()
    }

    /// The complication draws the last line and the state word, so the app keeps
    /// the shared snapshot current and asks the face to redraw. A turn start and
    /// a settle are the two moments worth a reload; the timeline's fifteen
    /// minutes covers a watch whose app has not run.
    private func publishToFace() {
        let line = turns.last(where: { $0.role == .assistant })?.text ?? ""
        WatchStateSnapshot.write(lastLine: line, stateWord: state.word)
        WidgetCenter.shared.reloadTimelines(ofKind: "KallistiComplication")
    }
}
