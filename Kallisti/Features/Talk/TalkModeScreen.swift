import SwiftUI

struct TalkModeScreen: View {
    @Environment(TalkStore.self) private var talkStore
    @Environment(AppSessionStore.self) private var sessionStore

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            VStack(spacing: Design.Spacing.xl) {
                Spacer()

                VoiceOrb(voiceState: talkStore.voiceState, connectionState: talkStore.connectionState)
                    .onTapGesture {
                        if talkStore.voiceState == .speaking {
                            talkStore.interruptAssistant()
                        }
                    }
                    .accessibilityAction(named: "Stop speaking") {
                        talkStore.interruptAssistant()
                    }

                TranscriptView(
                    transcriptItems: talkStore.transcriptItems,
                    voiceState: talkStore.voiceState
                )

                if let statusMessage = talkStore.statusMessage {
                    Text(statusMessage)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, Design.Spacing.lg)
                }

                if let blockedReason = talkStore.blockedReason, !talkStore.isSessionActive {
                    Text(blockedReason)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.warning)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, Design.Spacing.lg)
                }

                sessionTimer

                Spacer()

                controlBar
            }
            .padding(.bottom, Design.Spacing.xxl)
        }
        .navigationTitle("Talk Mode")
        .task {
            await talkStore.refreshReadiness()
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                mockIndicator
            }
        }
    }

    // MARK: - Session Timer

    private var sessionTimer: some View {
        Group {
            if talkStore.isSessionActive {
                Text("Session · \(formattedDuration)")
                    .brandEyebrow()
                    .transition(.opacity.combined(with: .scale(scale: 0.9)))
            }
        }
        .animation(Design.Motion.standard, value: talkStore.isSessionActive)
    }

    private var formattedDuration: String {
        let minutes = Int(talkStore.sessionDuration) / 60
        let seconds = Int(talkStore.sessionDuration) % 60
        return String(format: "%d:%02d", minutes, seconds)
    }

    // MARK: - Controls

    private var controlBar: some View {
        HStack(spacing: Design.Spacing.lg) {
            if talkStore.isSessionActive {
                // Mute button
                Button {
                    Task { await talkStore.toggleMute() }
                } label: {
                    Image(systemName: talkStore.isMuted ? "mic.slash.fill" : "mic.fill")
                        .font(.system(size: Design.Size.iconLarge))
                        .foregroundStyle(talkStore.isMuted ? Design.Colors.danger : Design.Colors.foreground)
                        .frame(width: Design.Size.minTapTarget, height: Design.Size.minTapTarget)
                        .background(Design.Colors.surface)
                        .overlay(Circle().stroke(Design.Colors.border, lineWidth: 1))
                        .clipShape(Circle())
                }
                .accessibilityLabel(talkStore.isMuted ? "Unmute" : "Mute")

                // End session button — signal-orange primary CTA
                Button {
                    endSession()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: Design.Size.iconLarge, weight: .semibold))
                        .foregroundStyle(Design.Colors.background)
                        .frame(width: Design.Size.iconHero, height: Design.Size.iconHero)
                        .background(Design.Brand.accent, in: Circle())
                }
                .accessibilityLabel("End session")
            } else {
                // Start session button
                Button {
                    startSession()
                } label: {
                    Label("Start Talking", systemImage: "mic.fill")
                        .labelStyle(.titleAndIcon)
                        .brandEyebrow(Design.Colors.background)
                        .padding(.horizontal, Design.Spacing.lg)
                        .padding(.vertical, Design.Spacing.md)
                }
                .background(Design.Brand.accent)
                .clipShape(Capsule())
                .accessibilityLabel("Start voice session")
                .disabled(!talkStore.canStartSession)
                .opacity(talkStore.canStartSession ? 1 : 0.5)
            }
        }
        .animation(Design.Motion.expressive, value: talkStore.isSessionActive)
    }

    // MARK: - Mock Indicator

    private var mockIndicator: some View {
        Text(sessionStore.state.isMockMode ? "MOCK" : "LIVE")
            .brandEyebrow(sessionStore.state.isMockMode ? Design.Colors.warning : Design.Colors.success)
            .padding(.horizontal, Design.Spacing.xs)
            .padding(.vertical, Design.Spacing.xxxs)
            .background(Design.Colors.surface)
            .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
            .clipShape(Capsule())
    }

    // MARK: - Actions

    private func startSession() {
        Task { await talkStore.startSession() }
    }

    private func endSession() {
        Task { await talkStore.endSession() }
    }
}
