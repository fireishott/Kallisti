//
//  WatchConversationScreen.swift
//  KallistiWatch
//
//  The one screen the wrist needs: an orb that says what is happening, the last
//  few lines, and a tap that means "my turn" or "stop". Nothing else lives here.
//

import SwiftUI

struct WatchConversationScreen: View {
    @Environment(\.scenePhase) private var scenePhase

    @State private var store = WatchConversationStore()
    @State private var recorder = WatchSpeechToText()
    @State private var player = WatchClausePlayer()

    let chooser: WatchTransportChooser
    let transport: (any WatchTransport)?
    /// True when a shortcut or a complication asked for the mic, not just the
    /// screen.
    var autoListen = false

    var body: some View {
        VStack(spacing: 6) {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(store.turns) { turn in
                            WatchTurnView(turn: turn)
                        }
                    }
                    .padding(.horizontal, 4)
                }
                .onChange(of: store.turns.count) {
                    guard let last = store.turns.last else { return }
                    withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }

            if case let .failed(reason) = store.state {
                Text(reason)
                    .font(.footnote)
                    .foregroundStyle(.orange)
                    .multilineTextAlignment(.center)
            }

            // The orb: one tap begins, ends the capture, or barges in. The word
            // underneath is the state, so VoiceOver hears what a sighted person
            // sees.
            Button(action: tap) {
                ZStack {
                    Circle().fill(tint.opacity(0.25))
                    Circle().stroke(tint, lineWidth: 2).padding(4)
                    Text(store.state.word)
                        .font(.caption2)
                        .minimumScaleFactor(0.6)
                        .padding(6)
                }
            }
            .buttonStyle(.plain)
            .frame(width: 96, height: 96)
            .accessibilityLabel("Kallisti voice")
            .accessibilityValue(store.state.word)

            if store.carrier != .none {
                Text(store.carrier.rawValue)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
            }
        }
        .onAppear {
            store.onClause = { wav in player.enqueue(wav) }
            if autoListen, store.state == .idle {
                tap()
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { store.wristLowered() }
        }
    }

    private var tint: Color {
        switch store.state {
        case .idle: .gray
        case .connecting: .blue
        case .listening: .green
        case .thinking: .purple
        case .speaking: .teal
        case .interrupted: .orange
        case .failed: .red
        }
    }

    private func tap() {
        switch store.state {
        case .idle, .failed:
            store.begin()
            startCapture()
        case .listening:
            recorder.stop()
        case .speaking:
            player.stop()
            store.bargeIn()
            store.resumeListening()
            startCapture()
        case .thinking, .connecting, .interrupted:
            break
        }
    }

    private func startCapture() {
        guard let transport else {
            store.fail("no_transport")
            return
        }
        recorder.start { clip in
            guard let clip, !clip.isEmpty else {
                store.fail("empty_clip")
                return
            }
            Task {
                do {
                    // The wrist does not transcribe: it records and the daemon
                    // does, so the clip never leaves the host.
                    let text = try await transport.transcribe(audio: clip)
                    await store.send(text, using: chooser)
                    store.follow(using: transport)
                } catch let error as WatchTransportError {
                    store.fail(error.word)
                } catch {
                    store.fail("unreachable")
                }
            }
        }
    }
}
